//! EFF-001 admission and PTY-002 lease fencing guarantees.
use rightkit_control::admission::{
    AdmissionDecision, AdmissionHook, CancelToken, EffectAdmissionStage as Stage, EffectGate,
    EffectKind, EffectRequest, ExecError, SettleOutcome,
};
use rightkit_control::events::{ControlEvent, EventSink};
use rightkit_control::lease::{
    ControlSession, InputError, InputFailure, InputLease, InputOutcome, LeaseError,
    UnacknowledgedInputDecision,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

fn recorder() -> (EventSink, Arc<Mutex<Vec<ControlEvent>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    (
        Arc::new(move |e: &ControlEvent| l.lock().unwrap().push(e.clone())),
        log,
    )
}

fn req(method: &str) -> EffectRequest {
    EffectRequest::new(EffectKind::Input, method, "main").with_params(r#"{"text":"secret"}"#)
}

fn deny_all(_: &EffectRequest) -> AdmissionDecision {
    AdmissionDecision::deny("policy says no")
}

// ---------- EFF-001 ----------

#[test]
fn denied_request_has_no_side_effect() {
    let (sink, log) = recorder();
    let gate = EffectGate::new(deny_all).with_events(sink);
    let side_effects = AtomicUsize::new(0);
    let effect = gate.admit(req("type"), &CancelToken::new(), |_| {
        side_effects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(
        side_effects.load(Ordering::SeqCst),
        0,
        "executor ran after denial"
    );
    let st = &effect.settlement;
    assert_eq!(st.outcome, SettleOutcome::Denied);
    assert!(!st.executed);
    assert_eq!(st.stages, [Stage::Validate, Stage::Approval, Stage::Settle]);
    assert_eq!(st.reason.as_deref(), Some("policy says no"));
    assert!(effect.value.is_none());
    let events = log.lock().unwrap().clone();
    assert_eq!(events.len(), 1);
    assert!(
        matches!(&events[0], ControlEvent::EffectDenied { stage: Stage::Approval, reason, .. } if reason == "policy says no")
    );
    // Events are content-free: the payload never appears.
    assert!(!format!("{events:?}").contains("secret"));
    assert_eq!(gate.settlements(), vec![effect.settlement.clone()]);
}

struct Validating(Arc<AtomicUsize>);
impl AdmissionHook for Validating {
    fn validate(&self, r: &EffectRequest) -> Result<(), String> {
        if r.target.is_empty() {
            Err("no target".into())
        } else {
            Ok(())
        }
    }
    fn approve(&self, _: &EffectRequest) -> AdmissionDecision {
        self.0.fetch_add(1, Ordering::SeqCst);
        AdmissionDecision::Allow
    }
}

#[test]
fn validation_denies_before_approval_and_execute() {
    let approvals = Arc::new(AtomicUsize::new(0));
    let gate = EffectGate::new(Validating(approvals.clone()));
    let ran = AtomicUsize::new(0);
    let e = gate.admit(
        EffectRequest::new(EffectKind::Action, "press", ""),
        &CancelToken::new(),
        |_| {
            ran.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    );
    assert_eq!(e.settlement.outcome, SettleOutcome::Denied);
    assert_eq!(e.settlement.stages, [Stage::Validate, Stage::Settle]);
    assert_eq!(approvals.load(Ordering::SeqCst), 0);
    assert_eq!(ran.load(Ordering::SeqCst), 0);
    // Empty method is refused by the crate itself.
    let e = EffectGate::allow_all().admit(
        EffectRequest::new(EffectKind::Input, " ", "main"),
        &CancelToken::new(),
        |_| {
            ran.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    );
    assert_eq!(e.settlement.outcome, SettleOutcome::Denied);
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}

#[test]
fn panicking_hook_fails_closed() {
    let gate = EffectGate::new(|_: &EffectRequest| -> AdmissionDecision { panic!("hook bug") });
    let ran = AtomicUsize::new(0);
    let e = gate.admit(req("click"), &CancelToken::new(), |_| {
        ran.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(e.settlement.outcome, SettleOutcome::Denied);
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}

#[test]
fn every_outcome_settles_with_lifecycle_events() {
    let (sink, log) = recorder();
    let gate = EffectGate::allow_all().with_events(sink);
    let ran = AtomicUsize::new(0);

    let ok = gate.admit(req("ok"), &CancelToken::new(), |_| {
        ran.fetch_add(1, Ordering::SeqCst);
        Ok(42)
    });
    assert_eq!(ok.settlement.outcome, SettleOutcome::Ok);
    assert_eq!(
        ok.settlement.stages,
        [
            Stage::Validate,
            Stage::Approval,
            Stage::Execute,
            Stage::Settle
        ]
    );
    assert_eq!(ok.into_result(), Ok(42));

    let failed = gate.admit(req("fail"), &CancelToken::new(), |_| -> Result<(), _> {
        ran.fetch_add(1, Ordering::SeqCst);
        Err(ExecError::from("window gone"))
    });
    assert_eq!(failed.settlement.outcome, SettleOutcome::Failed);
    assert!(failed.settlement.executed && !failed.settlement.crashed);
    assert_eq!(failed.into_result(), Err("failed: window gone".to_string()));

    let pre = CancelToken::new();
    pre.cancel();
    let cancelled_early = gate.admit(req("early"), &pre, |_| {
        ran.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(cancelled_early.settlement.outcome, SettleOutcome::Cancelled);
    assert!(!cancelled_early.settlement.executed);

    let mid = CancelToken::new();
    let cancelled_mid = gate.admit(req("mid"), &mid, |c| -> Result<(), _> {
        ran.fetch_add(1, Ordering::SeqCst);
        mid.cancel();
        assert!(c.is_cancelled());
        Err(ExecError::Cancelled)
    });
    assert_eq!(cancelled_mid.settlement.outcome, SettleOutcome::Cancelled);
    assert!(cancelled_mid.settlement.executed);

    let crashed = gate.admit(req("crash"), &CancelToken::new(), |_| -> Result<(), _> {
        ran.fetch_add(1, Ordering::SeqCst);
        panic!("executor bug")
    });
    assert_eq!(crashed.settlement.outcome, SettleOutcome::Failed);
    assert!(crashed.settlement.crashed);
    assert!(crashed
        .settlement
        .reason
        .as_deref()
        .unwrap()
        .contains("executor bug"));

    let denied = EffectGate::new(deny_all);
    let _ = denied.admit(req("x"), &CancelToken::new(), |_| Ok(()));

    // ok, fail, mid, crash executed; early did not.
    assert_eq!(ran.load(Ordering::SeqCst), 4);
    let outcomes: Vec<_> = gate.settlements().iter().map(|s| s.outcome).collect();
    assert_eq!(
        outcomes,
        [
            SettleOutcome::Ok,
            SettleOutcome::Failed,
            SettleOutcome::Cancelled,
            SettleOutcome::Cancelled,
            SettleOutcome::Failed
        ]
    );
    for s in gate.settlements() {
        assert_eq!(s.stages.last(), Some(&Stage::Settle));
        assert!(
            s.stages.windows(2).all(|w| w[0] < w[1]),
            "non-canonical {:?}",
            s.stages
        );
    }
    let names: Vec<String> = log
        .lock()
        .unwrap()
        .iter()
        .map(|e| match e {
            ControlEvent::EffectStarted { method, .. } => format!("start:{method}"),
            ControlEvent::EffectStopped {
                method, outcome, ..
            } => {
                format!("stop:{method}:{}", outcome.label())
            }
            ControlEvent::EffectCrashed { method, .. } => format!("crash:{method}"),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        names,
        [
            "start:ok",
            "stop:ok:ok",
            "start:fail",
            "stop:fail:failed",
            "stop:early:cancelled",
            "start:mid",
            "stop:mid:cancelled",
            "start:crash",
            "crash:crash",
        ]
    );
}

#[test]
fn journal_is_bounded_and_ids_are_unique() {
    let gate = EffectGate::allow_all().with_journal_capacity(3);
    for _ in 0..5 {
        let _ = gate.admit(req("k"), &CancelToken::new(), |_| Ok(()));
    }
    let ids: Vec<u64> = gate.settlements().iter().map(|s| s.id).collect();
    assert_eq!(ids, [3, 4, 5]);
}

#[test]
fn concurrent_admission_never_executes_denied_requests() {
    let gate = Arc::new(
        EffectGate::new(|r: &EffectRequest| {
            if r.id.is_multiple_of(2) {
                AdmissionDecision::deny("even")
            } else {
                AdmissionDecision::Allow
            }
        })
        .with_journal_capacity(10_000),
    );
    let executed = Arc::new(Mutex::new(Vec::new()));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let (gate, executed) = (gate.clone(), executed.clone());
            std::thread::spawn(move || {
                for _ in 0..250 {
                    let _ = gate.admit(req("c"), &CancelToken::new(), |_| {
                        executed.lock().unwrap().push(());
                        Ok(())
                    });
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let settled = gate.settlements();
    assert_eq!(settled.len(), 2000);
    let ok = settled
        .iter()
        .filter(|s| s.outcome == SettleOutcome::Ok)
        .count();
    let denied = settled
        .iter()
        .filter(|s| s.outcome == SettleOutcome::Denied && !s.executed && s.id.is_multiple_of(2))
        .count();
    assert_eq!((ok, denied), (1000, 1000));
    assert_eq!(executed.lock().unwrap().len(), ok);
}

// ---------- PTY-002 ----------

#[test]
fn input_requires_a_lease() {
    let lease = InputLease::new("pty");
    let ran = AtomicUsize::new(0);
    let r = lease.submit(0, 0, || {
        ran.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(r, Err(InputError::Lease(LeaseError::NotAttached)));
    let g = lease.acquire().unwrap();
    assert_eq!(
        (g.epoch, g.next_input_sequence, g.fenced_epoch),
        (1, 0, None)
    );
    lease.release(1).unwrap();
    assert_eq!(
        lease.submit(1, 0, || Ok(())),
        Err(InputError::Lease(LeaseError::NotAttached))
    );
    assert_eq!(ran.load(Ordering::SeqCst), 0);
}

#[test]
fn takeover_fences_the_old_holder() {
    let (sink, log) = recorder();
    let lease = InputLease::new("win").with_events(sink);
    let ran = AtomicUsize::new(0);
    let old = lease.acquire().unwrap();
    assert!(lease
        .submit(old.epoch, 0, || Ok(ran.fetch_add(1, Ordering::SeqCst)))
        .is_ok());

    let new = lease.acquire().unwrap();
    assert_eq!(new.epoch, old.epoch + 1);
    assert_eq!(new.fenced_epoch, Some(old.epoch));
    assert_eq!(
        new.next_input_sequence, 1,
        "sequence continues across epochs"
    );

    // Old holder: next in-order input, and a duplicate, are both stale-rejected.
    for seq in [1, 0] {
        assert_eq!(
            lease.submit(old.epoch, seq, || Ok(ran.fetch_add(1, Ordering::SeqCst))),
            Err(InputError::Lease(LeaseError::StaleEpoch {
                presented: 1,
                current: 2
            }))
        );
    }
    assert_eq!(
        lease.release(old.epoch).unwrap_err(),
        LeaseError::StaleEpoch {
            presented: 1,
            current: 2
        }
    );
    assert_eq!(
        lease.submit(9, 1, || Ok(0)),
        Err(InputError::Lease(LeaseError::FutureEpoch {
            presented: 9,
            current: 2
        }))
    );
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    assert!(lease
        .submit(new.epoch, 1, || Ok(ran.fetch_add(1, Ordering::SeqCst)))
        .is_ok());
    assert_eq!(ran.load(Ordering::SeqCst), 2);

    let events = log.lock().unwrap().clone();
    assert!(events.contains(&ControlEvent::LeaseFenced {
        lease: "win".into(),
        old_epoch: 1,
        new_epoch: 2
    }));
    assert!(events.iter().any(|e| matches!(
        e,
        ControlEvent::InputRejected {
            epoch: 1,
            reason: "stale_epoch",
            ..
        }
    )));
}

#[test]
fn sequences_are_acked_deduplicated_and_ordered() {
    let lease = InputLease::new("pty");
    let g = lease.acquire().unwrap();
    let applied = Mutex::new(Vec::new());
    let send = |seq: u64| {
        lease.submit(g.epoch, seq, || {
            applied.lock().unwrap().push(seq);
            Ok(seq * 10)
        })
    };
    assert_eq!(
        send(0).unwrap(),
        InputOutcome::Applied {
            ack: rightkit_control::lease::InputAck {
                epoch: 1,
                sequence: 0
            },
            value: 0
        }
    );
    assert!(matches!(
        send(1).unwrap(),
        InputOutcome::Applied { value: 10, .. }
    ));
    // Duplicate: re-acknowledged, not re-executed.
    let dup = send(0).unwrap();
    assert!(matches!(dup, InputOutcome::Duplicate { .. }));
    assert_eq!(dup.ack().sequence, 0);
    // Gap: rejected, not buffered.
    assert_eq!(
        send(3),
        Err(InputError::Lease(LeaseError::OutOfOrder {
            expected: 2,
            got: 3
        }))
    );
    assert!(lease.check(g.epoch, 2).unwrap());
    assert!(!lease.check(g.epoch, 1).unwrap());
    assert!(send(2).is_ok());
    assert_eq!(*applied.lock().unwrap(), [0, 1, 2]);
    let snap = lease.snapshot();
    assert_eq!(
        (
            snap.next_input_sequence,
            snap.acked_input_sequence,
            snap.unacknowledged_input
        ),
        (3, 3, None)
    );
    assert_eq!(snap.last_acked_input_sequence(), Some(2));
}

#[test]
fn not_delivered_releases_the_sequence() {
    let lease = InputLease::new("pty");
    let g = lease.acquire().unwrap();
    assert_eq!(
        lease.submit(g.epoch, 0, || -> Result<(), _> {
            Err(InputFailure::NotDelivered("denied".into()))
        }),
        Err(InputError::NotDelivered {
            reason: "denied".into()
        })
    );
    assert_eq!(lease.snapshot().unacknowledged_input, None);
    assert!(lease.submit(g.epoch, 0, || Ok(())).is_ok());
}

#[test]
fn ambiguous_input_must_be_reconciled_explicitly() {
    let lease = InputLease::new("pty");
    let g = lease.acquire().unwrap();
    assert!(lease.submit(g.epoch, 0, || Ok(())).is_ok());
    let r = lease.submit(g.epoch, 1, || -> Result<(), _> { panic!("pipe broke") });
    assert!(matches!(
        r,
        Err(InputError::Ambiguous {
            unacknowledged: (1, 2),
            ..
        })
    ));
    // No new input, no resend of the same sequence, no plain reattach.
    for seq in [1, 2] {
        assert_eq!(
            lease.submit(g.epoch, seq, || Ok(())),
            Err(InputError::Lease(LeaseError::UnacknowledgedInput {
                from: 1,
                to: 2
            }))
        );
    }
    assert_eq!(
        lease.acquire().unwrap_err(),
        LeaseError::UnacknowledgedInput { from: 1, to: 2 }
    );
    // Acked history is still deduplicated.
    assert!(matches!(
        lease.submit(g.epoch, 0, || Ok(())),
        Ok(InputOutcome::Duplicate { .. })
    ));

    // New controller reconciles as lost: resend under the same, now-unambiguous sequence.
    let g2 = lease.acquire_reconciling(UnacknowledgedInputDecision::ReconcileAsLost);
    assert_eq!(
        (g2.epoch, g2.next_input_sequence, g2.fenced_epoch),
        (2, 1, Some(1))
    );
    assert!(lease.submit(g2.epoch, 1, || Ok(())).is_ok());

    // Holder reconciles in place as delivered.
    let _ = lease.submit(g2.epoch, 2, || -> Result<(), _> {
        Err(InputFailure::Ambiguous("timeout".into()))
    });
    assert_eq!(
        lease
            .reconcile(g.epoch, UnacknowledgedInputDecision::ReconcileAsDelivered)
            .unwrap_err(),
        LeaseError::StaleEpoch {
            presented: 1,
            current: 2
        }
    );
    let snap = lease
        .reconcile(g2.epoch, UnacknowledgedInputDecision::ReconcileAsDelivered)
        .unwrap();
    assert_eq!(
        (snap.next_input_sequence, snap.acked_input_sequence),
        (3, 3)
    );
    assert!(matches!(
        lease.submit(g2.epoch, 2, || Ok(())),
        Ok(InputOutcome::Duplicate { .. })
    ));
}

#[test]
fn session_fences_before_admission_and_denial_releases_sequence() {
    let hook_calls = Arc::new(AtomicUsize::new(0));
    let hc = hook_calls.clone();
    let gate = Arc::new(EffectGate::new(move |r: &EffectRequest| {
        hc.fetch_add(1, Ordering::SeqCst);
        if r.method == "blocked" {
            AdmissionDecision::deny("no")
        } else {
            AdmissionDecision::Allow
        }
    }));
    let session = ControlSession::new(Arc::new(InputLease::new("app")), gate.clone());
    let side_effects = AtomicUsize::new(0);
    let fx = |_: &CancelToken| -> Result<(), ExecError> {
        side_effects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    };
    let old = session.lease.acquire().unwrap();
    let new = session.lease.acquire().unwrap();

    // Stale controller never reaches the hook.
    let r = session.input(old.epoch, 0, req("type"), &CancelToken::new(), fx);
    assert!(matches!(
        r,
        Err(InputError::Lease(LeaseError::StaleEpoch { .. }))
    ));
    assert_eq!(hook_calls.load(Ordering::SeqCst), 0);

    // Denied: no side effect, sequence released for reuse.
    let r = session.input(new.epoch, 0, req("blocked"), &CancelToken::new(), fx);
    assert!(
        matches!(r, Err(InputError::NotDelivered { ref reason }) if reason.starts_with("denied"))
    );
    assert_eq!(side_effects.load(Ordering::SeqCst), 0);
    assert!(session
        .input(new.epoch, 0, req("type"), &CancelToken::new(), fx)
        .is_ok());
    // Duplicate never re-admitted or re-executed.
    assert!(matches!(
        session.input(new.epoch, 0, req("type"), &CancelToken::new(), fx),
        Ok(InputOutcome::Duplicate { .. })
    ));
    assert_eq!(side_effects.load(Ordering::SeqCst), 1);
    assert_eq!(hook_calls.load(Ordering::SeqCst), 2);

    // Executor failure after start is ambiguous.
    let r = session.input(
        new.epoch,
        1,
        req("type"),
        &CancelToken::new(),
        |_| -> Result<(), _> { Err(ExecError::from("lost")) },
    );
    assert!(matches!(
        r,
        Err(InputError::Ambiguous {
            unacknowledged: (1, 2),
            ..
        })
    ));
    let outcomes: Vec<_> = gate.settlements().iter().map(|s| s.outcome).collect();
    assert_eq!(
        outcomes,
        [
            SettleOutcome::Denied,
            SettleOutcome::Ok,
            SettleOutcome::Failed
        ]
    );
}

#[test]
fn concurrent_controllers_never_interleave_stale_input() {
    let lease = Arc::new(InputLease::new("shared"));
    // (epoch, sequence) of every executed input, in execution order.
    let applied = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let (lease, applied) = (lease.clone(), applied.clone());
            std::thread::spawn(move || {
                let mut ok = 0usize;
                for _ in 0..50 {
                    let Ok(g) = lease.acquire() else { continue };
                    let mut seq = g.next_input_sequence;
                    for _ in 0..5 {
                        let applied = applied.clone();
                        match lease.submit(g.epoch, seq, move || {
                            applied.lock().unwrap().push((g.epoch, seq));
                            Ok(())
                        }) {
                            Ok(InputOutcome::Applied { .. }) => {
                                ok += 1;
                                seq += 1;
                            }
                            Ok(InputOutcome::Duplicate { .. }) => panic!("fresh seq deduplicated"),
                            Err(InputError::Lease(LeaseError::StaleEpoch { .. })) => break,
                            Err(InputError::Lease(LeaseError::OutOfOrder { .. })) => {
                                panic!("out of order within one epoch")
                            }
                            Err(e) => panic!("unexpected {e}"),
                        }
                    }
                }
                ok
            })
        })
        .collect();
    let total: usize = threads.into_iter().map(|t| t.join().unwrap()).sum();
    let applied = applied.lock().unwrap();
    assert_eq!(
        applied.len(),
        total,
        "every execution was acknowledged exactly once"
    );
    // Sequences are exactly 0..n with no duplicate or gap, and epochs never go
    // backwards: a fenced holder never executed after its successor.
    for (i, (_, seq)) in applied.iter().enumerate() {
        assert_eq!(*seq, i as u64);
    }
    assert!(applied.windows(2).all(|w| w[0].0 <= w[1].0));
    let snap = lease.snapshot();
    assert_eq!(snap.next_input_sequence, total as u64);
    assert_eq!(snap.unacknowledged_input, None);
}
