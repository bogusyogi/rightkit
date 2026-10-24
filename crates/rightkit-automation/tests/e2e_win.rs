//! End-to-end on Windows against a real app (Notepad): launched without
//! activation, driven through UI Automation (tree walk, ValuePattern), window
//! messages (tier 3) and PrintWindow, closed again. Same scenarios as `e2e_mac`
//! for the platform-neutral guarantees: lease fencing, admission, credentials,
//! screenshot redaction, lifecycle events, frontmost unchanged.
//!
//! The file compiles on every OS (it only uses the portable API) so it is
//! type-checked on macOS; on a non-Windows host the test returns immediately.
//! STATUS: not run on Windows by the author. Windows host:
//!   cd D:\Claude\rightkit
//!   rustright cargo test -p rightkit-automation --test e2e_win -- --nocapture

#![allow(dead_code)]

use rightkit_automation::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

#[derive(Default)]
struct DenyKeys(Mutex<Vec<String>>);
impl Admission for DenyKeys {
    fn approve(&self, req: &EffectRequest) -> Admit {
        self.0.lock().unwrap().push(req.action.clone());
        if req.action == "keys" {
            Admit::Deny("keys are forbidden in this test".into())
        } else {
            Admit::Allow
        }
    }
}

#[test]
fn notepad_end_to_end() {
    if !cfg!(windows) {
        eprintln!("e2e_win: Windows-only gate; nothing to do on this OS");
        return;
    }
    run();
}

fn run() {
    // Launching Notepad steals focus, which the no-focus-change assertions below would then report: this
    // test runs only in a dedicated desktop session, which CI's gate marks with RIGHTKIT_REQUIRE_DESKTOP=1.
    // It never runs against a developer's live desktop.
    let require_desktop = std::env::var("RIGHTKIT_REQUIRE_DESKTOP").as_deref() == Ok("1");
    if !require_desktop {
        eprintln!("SKIP notepad_end_to_end: needs a dedicated desktop session (RIGHTKIT_REQUIRE_DESKTOP=1, set by CI); never run on a live user desktop");
        return;
    }
    reset_kill();
    let out = tempfile::tempdir().unwrap();
    let a = native(Policy::new(["notepad.exe"]));
    a.add_evidence(Arc::new(
        JsonlEvidence::open(&out.path().join("evidence.jsonl")).unwrap(),
    ));
    let journal = Arc::new(MemoryJournal::default());
    a.add_journal(journal.clone());
    let hook = Arc::new(DenyKeys::default());
    a.set_admission(hook.clone());
    // Real foreground measurement: pid AND top-level window, sampled continuously from
    // here to the end. `None == None` is not a pass: an unmeasurable foreground fails.
    let front0 = a.frontmost();
    // RIGHTKIT_REQUIRE_DESKTOP=1 is set (the test returned above otherwise), so a foreground that cannot
    // be measured (no interactive desktop, such as a hidden build broker) is a failure, never a skip.
    assert!(
        front0.is_some(),
        "foreground window could not be measured; run in an interactive desktop session"
    );
    let sampler = NativePlatform::new();
    let watch = FocusWatch::start(move || sampler.frontmost(), Duration::from_millis(15));
    let unchanged = |what: &str| {
        let f = a.frontmost();
        assert_eq!(
            f.as_ref().map(|f| (f.pid, f.window_id)),
            front0.as_ref().map(|f| (f.pid, f.window_id)),
            "foreground changed: {what} (was {:?}, now {:?})",
            front0.as_ref().map(|f| f.app_id.as_str()),
            f.as_ref().map(|f| f.app_id.as_str())
        );
    };

    // Allowlist: explorer.exe is never drivable.
    if let Some(x) = a.apps().into_iter().find(|x| x.app_id == "explorer.exe") {
        assert!(matches!(
            a.snapshot(Target::app(x.pid), Limits::default()),
            Err(Error::Blocked(_))
        ));
    }
    assert!(
        matches!(a.launch("calc.exe"), Err(Error::Blocked(_))),
        "a non-allowlisted app is refused before launch"
    );

    let app = a.launch("notepad.exe").expect("launch notepad");
    sleep(1500);
    unchanged("launch");
    a.begin_session(app.pid, "e2e_win").unwrap();
    let limits = Limits {
        max_depth: 12,
        max_nodes: 600,
    };
    let s = a.snapshot(Target::app(app.pid), limits).unwrap();
    let s2 = a.snapshot(Target::app(app.pid), limits).unwrap();
    assert_eq!(
        s.fingerprint, s2.fingerprint,
        "UIA tree is stable between reads"
    );
    assert!(s2.root.count() > 3);
    let editor = |s: &Snapshot| {
        s.root
            .find(&|n| n.role == "textarea" || n.role == "textfield")
            .cloned()
            .expect("editor control")
    };
    let ed = editor(&s2);
    unchanged("snapshots");

    // Tier 1 (ValuePattern), read back. Strict focus: an app that activates itself when UI
    // Automation writes to it is reported as `FocusMoved` and the previous app is put back.
    // The packaged Notepad on current Windows does that; the classic one does not.
    let tier1 = a.act(
        &s2.id,
        Some(&ed.id),
        Action::SetValue("tier1 value".into()),
        &ActOptions {
            guard: Guard::Element,
            strict_focus: true,
            ..Default::default()
        },
    );
    match tier1 {
        Ok(o) => assert_eq!(o.receipt.tier_used, Some(Tier::Semantic)),
        Err(Error::FocusMoved { .. }) => {
            sleep(150);
            unchanged("strict focus restores the foreground after tier 1");
            a.end_session(app.pid);
            let _ = a.terminate(app.pid);
            let reason = "the installed Notepad activates itself on a UI Automation write, \
                          so the frontmost-unchanged scenarios cannot run against it";
            if std::env::var_os("RIGHTKIT_REQUIRE_ASSETS").is_some_and(|v| v == "1") {
                panic!("e2e_win: {reason}");
            }
            println!("SKIP e2e_win: {reason}");
            return;
        }
        Err(e) => panic!("set value: {e:?}"),
    }
    sleep(150);
    let read = |a: &Automation<NativePlatform>| {
        editor(&a.snapshot(Target::app(app.pid), limits).unwrap())
            .value
            .unwrap_or_default()
    };
    assert_eq!(read(&a), "tier1 value");
    unchanged("tier 1");

    // Lease fencing with an acked sequence.
    let l1 = a
        .acquire_lease(app.pid, LeaseRequest::new("controller-a"))
        .unwrap();
    let s = a.snapshot(Target::app(app.pid), limits).unwrap();
    let ed = editor(&s);
    let f = |seq: u64, epoch: u64| ActOptions {
        fence: Some(Fence { epoch, seq }),
        guard: Guard::Element,
        ..Default::default()
    };
    assert!(
        matches!(
            a.act(
                &s.id,
                Some(&ed.id),
                Action::SetValue("no lease".into()),
                &f(1, 0)
            ),
            Err(Error::StaleLease { .. })
        ),
        "a made-up epoch is stale"
    );
    a.act(
        &s.id,
        Some(&ed.id),
        Action::SetValue("one".into()),
        &f(1, l1.epoch),
    )
    .unwrap();
    assert!(matches!(
        a.act(
            &s.id,
            Some(&ed.id),
            Action::SetValue("again".into()),
            &f(1, l1.epoch)
        ),
        Err(Error::Replayed { acked_seq: 1 })
    ));
    let l2 = a
        .acquire_lease(app.pid, LeaseRequest::new("controller-b").takeover())
        .unwrap();
    assert!(matches!(
        a.act(
            &s.id,
            Some(&ed.id),
            Action::SetValue("stale".into()),
            &f(2, l1.epoch)
        ),
        Err(Error::StaleLease { .. })
    ));
    sleep(150);
    assert_eq!(read(&a), "one", "the displaced controller cannot inject");
    assert!(a.release_lease(&l2));

    // Admission denies before any side effect.
    let s = a.snapshot(Target::app(app.pid), limits).unwrap();
    let ed = editor(&s);
    let denied = a
        .act(
            &s.id,
            Some(&ed.id),
            Action::Keys("x".into()),
            &ActOptions {
                guard: Guard::Off,
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(matches!(denied, Error::Denied(_)));
    sleep(100);
    assert_eq!(read(&a), "one");

    // Credentials are never typed.
    let secret = format!("{}{}", "sk", "-abcdefghijklmnopqrstuvwxyz0123");
    assert!(matches!(
        a.act(
            &s.id,
            Some(&ed.id),
            Action::SetValue(secret),
            &ActOptions {
                guard: Guard::Off,
                ..Default::default()
            }
        ),
        Err(Error::Blocked(_))
    ));
    assert_eq!(read(&a), "one");

    // Delivery with read-back proof.
    let pin = a.capture_pin(Some(app.pid)).expect("pin");
    let rec = a.deliver_text(&pin, " + delivered", &DeliveryOptions::default());
    assert!(rec.inserted(), "{:?}", rec.outcome);
    unchanged("delivery");

    // Screenshot redaction (PrintWindow output decodes and the region is black).
    let s = a.snapshot(Target::app(app.pid), limits).unwrap();
    a.act(
        &s.id,
        Some(&editor(&s).id),
        Action::SetValue("visible MARKER_ZEBRA tail".into()),
        &ActOptions {
            guard: Guard::Off,
            ..Default::default()
        },
    )
    .unwrap();
    sleep(250);
    a.add_redactor(Arc::new(MaskText(vec!["MARKER_ZEBRA".into()])));
    let shot = out.path().join("masked.png");
    let rep = a
        .screenshot_report(&Target::app(app.pid), &shot)
        .expect("screenshot");
    assert!(rep.masked >= 1);
    let (w, h, px) = png::decode_rgba(&std::fs::read(&shot).unwrap()).expect("decodable PNG");
    assert_eq!((w, h), (rep.width, rep.height));
    assert!(
        px.chunks(4).filter(|p| p[..3] == [0, 0, 0]).count() > (w * h / 20) as usize,
        "a large black region is present"
    );

    // The target exits: the journal says so.
    a.end_session(app.pid);
    let _ = a.terminate(app.pid);
    sleep(1200);
    assert!(
        !a.apps().iter().any(|x| x.pid == app.pid),
        "closed what it opened (a save prompt would leave it running)"
    );
    unchanged("end");
    let focus = watch.finish();
    assert!(
        focus.proven(),
        "no-focus-steal: {} ({focus:?})",
        focus.explain()
    );
    let kinds: Vec<&str> = journal.all().iter().map(|e| e.kind()).collect();
    for k in [
        "started",
        "stopped",
        "lease_granted",
        "lease_fenced",
        "denied",
    ] {
        assert!(kinds.contains(&k), "journal lacks {k}: {kinds:?}");
    }
    kill();
    assert!(matches!(
        a.snapshot(Target::app(1), limits),
        Err(Error::Blocked(_))
    ));
    reset_kill();
}
