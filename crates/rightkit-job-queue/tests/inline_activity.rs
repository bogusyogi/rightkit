//! Public API: the activity registry (label, progress, listing) needs neither a store nor
//! protocol features. Written without running Cargo; not yet run.
use rightkit_job_queue::{ActivityOptions, ActivityRegistry, InlineCancelRegistry, JOB_EXISTS};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[test]
fn list_is_in_registration_order_with_label_and_progress() {
    let registry = ActivityRegistry::new();
    assert!(registry.is_empty());
    let (a, _) = registry
        .register_with("zeta", ActivityOptions::new().label("Export video"))
        .unwrap();
    let (b, _) = registry
        .register_with(
            "alpha",
            ActivityOptions::new()
                .label("Index")
                .message("starting")
                .progress(0.25),
        )
        .unwrap();
    let (_c, _) = registry.register("plain").unwrap();
    let listed = registry.list();
    let ids: Vec<&str> = listed.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        ["zeta", "alpha", "plain"],
        "registration order, not id order"
    );
    assert!(listed[0].seq < listed[1].seq && listed[1].seq < listed[2].seq);
    assert_eq!(listed[0].label.as_deref(), Some("Export video"));
    assert_eq!(listed[0].progress, None);
    assert_eq!(listed[1].message.as_deref(), Some("starting"));
    assert_eq!(listed[1].progress, Some(0.25));
    assert_eq!(listed[2].label, None);
    assert!(listed.iter().all(|s| !s.cancelled));
    assert_eq!(registry.len(), 3);

    a.set_progress(Some(0.5));
    a.set_label(Some("Export video (2 of 3)"));
    b.report(Some(0.75), Some("halfway"));
    let a_now = registry.get("zeta").unwrap();
    assert_eq!(a_now.progress, Some(0.5));
    assert_eq!(a_now.label.as_deref(), Some("Export video (2 of 3)"));
    let b_now = registry.get("alpha").unwrap();
    assert_eq!(
        (b_now.progress, b_now.message.as_deref()),
        (Some(0.75), Some("halfway"))
    );
    b.set_message(None);
    assert_eq!(registry.get("alpha").unwrap().message, None);
    assert!(registry.get("missing").is_none());
}

#[test]
fn progress_is_clamped_and_nan_clears_it() {
    let registry = InlineCancelRegistry::new();
    let (guard, _) = registry
        .register_with("p", ActivityOptions::new().progress(7.0))
        .unwrap();
    assert_eq!(registry.get("p").unwrap().progress, Some(1.0));
    guard.set_progress(Some(-3.0));
    assert_eq!(registry.get("p").unwrap().progress, Some(0.0));
    guard.set_progress(Some(0.5));
    guard.set_progress(Some(f64::NAN));
    assert_eq!(registry.get("p").unwrap().progress, None);
}

#[test]
fn release_removes_from_list_and_stale_guard_cannot_touch_a_reused_id() {
    let registry = InlineCancelRegistry::new();
    let (first, _) = registry.register("job").unwrap();
    let first_seq = registry.get("job").unwrap().seq;
    drop(first);
    assert!(registry.list().is_empty());
    let (second, _) = registry
        .register_with("job", ActivityOptions::new().label("second"))
        .unwrap();
    let second_seq = registry.get("job").unwrap().seq;
    assert!(second_seq > first_seq, "a reused id is a new registration");
    drop(second);
    assert!(registry.get("job").is_none());
}

#[test]
fn cancel_shows_in_the_snapshot_and_a_supplied_flag_is_the_one_registered() {
    let registry = InlineCancelRegistry::new();
    let mine = Arc::new(AtomicBool::new(false));
    let (_guard, flag) = registry
        .register_with(
            "mine",
            ActivityOptions::new().cancel_flag(Arc::clone(&mine)),
        )
        .unwrap();
    assert!(Arc::ptr_eq(&flag, &mine));
    let (_other, other_flag) = registry.register("other").unwrap();
    assert!(registry.cancel("mine"));
    assert!(mine.load(Ordering::SeqCst), "cancel set the caller's flag");
    assert!(!other_flag.load(Ordering::SeqCst));
    let by_id: std::collections::HashMap<_, _> = registry
        .list()
        .into_iter()
        .map(|s| (s.id.clone(), s))
        .collect();
    assert!(by_id["mine"].cancelled);
    assert!(!by_id["other"].cancelled);
    // A flag already set reads as cancelled at once.
    let preset = Arc::new(AtomicBool::new(true));
    let (_p, _) = registry
        .register_with("preset", ActivityOptions::new().cancel_flag(preset))
        .unwrap();
    assert!(registry.get("preset").unwrap().cancelled);
}

#[test]
fn duplicate_and_empty_ids_still_fail_through_register_with() {
    let registry = InlineCancelRegistry::new();
    assert_eq!(
        registry
            .register_with("", ActivityOptions::new())
            .err()
            .unwrap()
            .code,
        "bad_input"
    );
    let (_g, _) = registry.register("dup").unwrap();
    assert_eq!(
        registry
            .register_with("dup", ActivityOptions::new().label("again"))
            .err()
            .unwrap()
            .code,
        JOB_EXISTS
    );
    assert_eq!(registry.len(), 1);
}

#[test]
fn revision_moves_on_every_listed_change_and_is_shared_by_clones() {
    let registry = InlineCancelRegistry::new();
    let clone = registry.clone();
    let r0 = registry.revision();
    let (guard, _) = registry.register("r").unwrap();
    let r1 = clone.revision();
    assert!(r1 > r0, "register");
    guard.set_progress(Some(0.1));
    let r2 = registry.revision();
    assert!(r2 > r1, "progress");
    assert!(registry.cancel("r"));
    let r3 = registry.revision();
    assert!(r3 > r2, "cancel");
    assert_eq!(registry.revision(), r3, "reading changes nothing");
    assert!(!registry.cancel("missing"));
    assert_eq!(registry.revision(), r3, "a miss changes nothing");
    drop(guard);
    assert!(registry.revision() > r3, "release");
}

#[test]
fn unwinding_work_releases_its_listing() {
    let registry = InlineCancelRegistry::new();
    let shared = registry.clone();
    let _ = std::panic::catch_unwind(move || {
        let (_guard, _) = shared
            .register_with("boom", ActivityOptions::new().label("Will fail"))
            .unwrap();
        panic!("request failed");
    });
    assert!(registry.list().is_empty());
}

#[test]
fn concurrent_updates_and_listing_are_consistent() {
    let registry = InlineCancelRegistry::new();
    let mut workers = Vec::new();
    for n in 0..8 {
        let registry = registry.clone();
        workers.push(std::thread::spawn(move || {
            let id = format!("job-{n}");
            let (guard, _) = registry
                .register_with(&id, ActivityOptions::new().label(format!("Job {n}")))
                .unwrap();
            for step in 0..=20 {
                guard.set_progress(Some(f64::from(step) / 20.0));
                for s in registry.list() {
                    if let Some(p) = s.progress {
                        assert!((0.0..=1.0).contains(&p));
                    }
                }
            }
            assert_eq!(registry.get(&id).unwrap().progress, Some(1.0));
        }));
    }
    for w in workers {
        w.join().unwrap();
    }
    assert!(registry.is_empty());
}
