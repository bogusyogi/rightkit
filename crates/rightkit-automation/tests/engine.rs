//! Engine behaviour against the SIMULATED desktop (`mock::MockPlatform`): ids,
//! freshness guard, tier ladder, safety gates, evidence, delivery semantics.
//!
//! These are not the end-to-end gates (those are `e2e_mac.rs` / `e2e_win.rs`, real
//! apps). They stay because no real harmless target exposes the states below
//! (a secure field, a destroyed-and-recreated editor, a lossy clipboard, an app
//! that acknowledges a write without applying it, a sheet that steals focus).

use rightkit_automation::mock::*;
use rightkit_automation::*;
use std::sync::Arc;

fn rig() -> (
    Automation<MockPlatform>,
    MockPlatform,
    Arc<MemoryEvidence>,
    Arc<MemoryIndicator>,
) {
    let mock = MockPlatform::fixture();
    let a = Automation::new(mock.clone(), Policy::new(["com.test.editor"]));
    let ev = Arc::new(MemoryEvidence::default());
    let ind = Arc::new(MemoryIndicator::default());
    a.add_evidence(ev.clone());
    a.set_indicator(ind.clone());
    (a, mock, ev, ind)
}

fn id_of(snap: &Snapshot, name: &str) -> String {
    snap.root
        .find(&|n| n.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no node {name}"))
        .id
        .clone()
}

fn opts() -> ActOptions {
    ActOptions::default()
}

#[test]
fn snapshot_has_stable_unique_ids_and_focus() {
    let (a, _m, _e, _i) = rig();
    let s1 = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let s2 = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    assert_eq!(s1.root.count(), 5);
    assert_eq!(s1.fingerprint, s2.fingerprint);
    assert_ne!(s1.id, s2.id);
    for n in ["Save", "Delete", "Body", "Password"] {
        assert_eq!(id_of(&s1, n), id_of(&s2, n), "id for {n} must be stable");
    }
    let mut ids = std::collections::HashSet::new();
    fn walk(n: &Node, ids: &mut std::collections::HashSet<String>) {
        assert!(ids.insert(n.id.clone()), "duplicate id {}", n.id);
        n.children.iter().for_each(|c| walk(c, ids));
    }
    walk(&s1.root, &mut ids);
    assert_eq!(s1.focused, Some(id_of(&s1, "Body")));
    assert!(!s1.truncated);
    // secure values are never read into the tree
    assert_eq!(s1.root.find(&|n| n.secure).unwrap().value, None);
}

#[test]
fn snapshot_respects_limits() {
    let (a, _m, _e, _i) = rig();
    let s = a
        .snapshot(
            Target::app(EDITOR_PID),
            Limits {
                max_depth: 0,
                max_nodes: 100,
            },
        )
        .unwrap();
    assert_eq!(s.root.count(), 1);
    let s = a
        .snapshot(
            Target::app(EDITOR_PID),
            Limits {
                max_depth: 5,
                max_nodes: 3,
            },
        )
        .unwrap();
    assert!(s.truncated);
    assert_eq!(s.root.count(), 3);
}

#[test]
fn press_uses_tier1_with_no_events_and_unchanged_frontmost() {
    let (a, m, ev, ind) = rig();
    a.begin_session(EDITOR_PID, "test agent").unwrap();
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let out = a
        .act(&s.id, Some(&id_of(&s, "Save")), Action::Press, &opts())
        .unwrap();
    assert!(out.done());
    assert_eq!(out.receipt.tier_used, Some(Tier::Semantic));
    assert!(!out.receipt.focus_moved);
    assert_eq!(out.receipt.frontmost_before, Some(OTHER_PID));
    assert_eq!(out.receipt.frontmost_after, Some(OTHER_PID));
    assert_eq!(m.with(|s| s.pressed.clone()), vec![MockPlatform::SAVE]);
    assert!(m
        .log()
        .iter()
        .all(|l| !l.starts_with("post:") && !l.starts_with("foreground:")));
    assert_eq!(ev.all().len(), 1);
    assert!(ev.all()[0].to_json().contains("\"tier_used\":\"semantic\""));
    let events = ind.all();
    assert!(matches!(events[0], SessionEvent::Started { .. }));
    assert!(matches!(
        events[1],
        SessionEvent::Acted {
            tier: Some(Tier::Semantic),
            result: ResultKind::Done,
            ..
        }
    ));
    a.end_session(EDITOR_PID);
    assert!(matches!(ind.all().last(), Some(SessionEvent::Ended { .. })));
}

#[test]
fn snapshot_id_survives_many_acts() {
    let (a, _m, _e, _i) = rig();
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    for _ in 0..20 {
        assert!(a
            .act(&s.id, Some(&id_of(&s, "Save")), Action::Press, &opts())
            .unwrap()
            .done());
    }
}

#[test]
fn stale_snapshot_returns_fresh_and_does_nothing() {
    let (a, m, _e, _i) = rig();
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    m.add_button("Extra");
    let out = a
        .act(&s.id, Some(&id_of(&s, "Save")), Action::Press, &opts())
        .unwrap();
    assert_eq!(out.receipt.result, ResultKind::Changed);
    let fresh = out.fresh.expect("fresh snapshot");
    assert_eq!(fresh.root.count(), 6);
    assert!(
        m.with(|s| s.pressed.is_empty()),
        "nothing may be pressed on a stale snapshot"
    );
    // acting on the fresh snapshot works
    let out = a
        .act(
            &fresh.id,
            Some(&id_of(&fresh, "Save")),
            Action::Press,
            &opts(),
        )
        .unwrap();
    assert!(out.done());
    // Element-only guard tolerates unrelated structural change
    m.add_button("Another");
    let o = ActOptions {
        guard: Guard::Element,
        ..opts()
    };
    assert!(a
        .act(&fresh.id, Some(&id_of(&fresh, "Save")), Action::Press, &o)
        .unwrap()
        .done());
}

#[test]
fn element_guard_catches_a_renamed_element() {
    let (a, m, _e, _i) = rig();
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    m.with(|st| st.els[MockPlatform::SAVE].raw.name = Some("Something else".into()));
    let o = ActOptions {
        guard: Guard::Element,
        ..opts()
    };
    let out = a
        .act(&s.id, Some(&id_of(&s, "Save")), Action::Press, &o)
        .unwrap();
    assert_eq!(out.receipt.result, ResultKind::Changed);
}

#[test]
fn ladder_climbs_only_as_far_as_allowed_and_records_every_attempt() {
    let (a, m, _e, _i) = rig();
    m.with(|s| s.semantic_ok = false);
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let save = id_of(&s, "Save");
    // capped at semantic: nothing can do it
    let o = ActOptions {
        max_tier: Tier::Semantic,
        ..opts()
    };
    let err = a.act(&s.id, Some(&save), Action::Press, &o).unwrap_err();
    assert!(matches!(err, Error::NoTier(_)), "{err}");
    // default cap (targeted) clicks the element centre by pid-addressed event
    let out = a.act(&s.id, Some(&save), Action::Press, &opts()).unwrap();
    assert_eq!(out.receipt.tier_used, Some(Tier::Targeted));
    assert_eq!(out.receipt.attempts.len(), 3, "semantic, in_app, targeted");
    assert!(
        !out.receipt.attempts[0].ok && !out.receipt.attempts[1].ok && out.receipt.attempts[2].ok
    );
    assert!(m.log().iter().any(|l| l == "post:100:click(50,20)"));
    // foreground is never used unless asked
    m.with(|s| s.post_steals_focus = false);
    assert!(!m.log().iter().any(|l| l.starts_with("foreground:")));
}

#[test]
fn foreground_requires_opt_in_and_restores_focus() {
    let (a, m, _e, _i) = rig();
    m.with(|s| {
        s.semantic_ok = false;
        s.post_fails = true;
    });
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let save = id_of(&s, "Save");
    // Default cap stops at tier 3: refused, and the global queue is never touched.
    assert!(a.act(&s.id, Some(&save), Action::Press, &opts()).is_err());
    assert!(m
        .log()
        .iter()
        .all(|l| !l.starts_with("foreground:") && !l.starts_with("activate:")));
    // Explicit opt-in: tier 4 runs, activates, then gives focus back.
    let o = ActOptions {
        max_tier: Tier::Foreground,
        ..opts()
    };
    let out = a.act(&s.id, Some(&save), Action::Press, &o).unwrap();
    assert_eq!(out.receipt.tier_used, Some(Tier::Foreground));
    assert!(
        !out.receipt.focus_moved,
        "foreground tier is expected to move focus and restore it"
    );
    assert_eq!(a.frontmost().unwrap().pid, OTHER_PID);
    let log = m.log();
    let act = log.iter().position(|l| l == "activate:100").unwrap();
    let fg = log
        .iter()
        .position(|l| l.starts_with("foreground:100:click"))
        .unwrap();
    let back = log.iter().rposition(|l| l == "activate:200").unwrap();
    assert!(act < fg && fg < back);
}

#[test]
fn risky_actions_need_confirmation() {
    let (a, m, ev, _i) = rig();
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let del = id_of(&s, "Delete");
    let err = a
        .act(&s.id, Some(&del), Action::Press, &opts())
        .unwrap_err();
    assert_eq!(err, Error::NeedsConfirmation(RiskClass::Delete));
    assert!(m.with(|s| s.pressed.is_empty()));
    assert_eq!(ev.all().last().unwrap().result, ResultKind::Blocked);
    let o = ActOptions {
        confirm: vec![RiskClass::Delete],
        ..opts()
    };
    assert!(a.act(&s.id, Some(&del), Action::Press, &o).unwrap().done());
    // menu items are classified too
    let err = a
        .act(
            &s.id,
            None,
            Action::MenuPress(vec!["File".into(), "Close".into()]),
            &opts(),
        )
        .unwrap_err();
    assert_eq!(err, Error::NeedsConfirmation(RiskClass::Close));
}

#[test]
fn secure_fields_are_never_typed_into_and_never_logged() {
    let (a, m, ev, _i) = rig();
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let pw = id_of(&s, "Password");
    for act in [
        Action::SetValue("hunter2".into()),
        Action::TypeText("hunter2".into()),
        Action::Keys("a".into()),
    ] {
        let err = a.act(&s.id, Some(&pw), act, &opts()).unwrap_err();
        assert!(matches!(err, Error::Blocked(_)));
    }
    assert!(m.with(|s| s.els[MockPlatform::PASSWORD]
        .text
        .clone()
        .unwrap()
        .is_empty()));
    assert!(ev.all().iter().all(|r| !r.to_json().contains("hunter2")));
    // typing at the focus is refused when the focus is the password field
    m.with(|st| st.focused = Some(MockPlatform::PASSWORD));
    let err = a
        .act(&s.id, None, Action::TypeText("x".into()), &opts())
        .unwrap_err();
    assert!(matches!(err, Error::Blocked(_)));
}

#[test]
fn payloads_are_redacted_in_receipts() {
    let (a, _m, ev, _i) = rig();
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let body = id_of(&s, "Body");
    a.act(
        &s.id,
        Some(&body),
        Action::SetValue("hello world".into()),
        &opts(),
    )
    .unwrap();
    assert_eq!(ev.all()[0].value.as_deref(), Some("<11 chars>"));
    let o = ActOptions {
        log_values: true,
        ..opts()
    };
    a.act(
        &s.id,
        Some(&body),
        Action::SetValue("hello again".into()),
        &o,
    )
    .unwrap();
    assert_eq!(ev.all()[1].value.as_deref(), Some("hello again"));
    // A credential-shaped payload is refused outright (never typed), so it can never reach a receipt.
    let secret = format!("{}{}", "sk", "-abcdefghijklmnopqrstuvwxyz");
    let n = ev.all().len();
    let err = a
        .act(&s.id, Some(&body), Action::SetValue(secret.clone()), &o)
        .unwrap_err();
    assert!(matches!(err, Error::Blocked(_)));
    let last = ev.all().pop().unwrap();
    assert_eq!(ev.all().len(), n + 1, "the refusal is still recorded");
    assert_eq!(last.result, ResultKind::Blocked);
    assert_eq!(last.denied_by, Some(DeniedBy::Safety));
    assert!(
        !last.to_json().contains("abcdefghij"),
        "the credential text is in no receipt"
    );
}

#[test]
fn allowlist_and_deny_list_gate_every_entry_point() {
    let (a, m, _e, _i) = rig();
    assert!(matches!(
        a.snapshot(Target::app(OTHER_PID), Limits::default()),
        Err(Error::Blocked(_))
    ));
    assert!(matches!(
        a.begin_session(OTHER_PID, "x"),
        Err(Error::Blocked(_))
    ));
    assert!(matches!(a.focused(OTHER_PID), Err(Error::Blocked(_))));
    assert!(matches!(a.terminate(OTHER_PID), Err(Error::Blocked(_))));
    assert!(matches!(
        a.screenshot(
            &Target::app(OTHER_PID),
            &std::env::temp_dir().join("nonexistent").join("x.png")
        ),
        Err(Error::Blocked(_))
    ));
    // launching a non-allowlisted app is refused before launch
    let err = a.launch("com.test.stranger").unwrap_err();
    assert!(matches!(err, Error::Blocked(_)));
    assert!(
        !m.log().iter().any(|l| l.starts_with("launch:")),
        "refused before anything was launched: {:?}",
        m.log()
    );
    assert!(a
        .platform()
        .apps()
        .iter()
        .all(|x| x.app_id != "com.test.stranger"));
    // unknown snapshot
    assert!(matches!(
        a.act("nope", None, Action::Focus, &opts()),
        Err(Error::UnknownSnapshot(_))
    ));
}

#[test]
fn launch_and_terminate_allowlisted_app() {
    let mock = MockPlatform::fixture();
    let a = Automation::new(mock.clone(), Policy::new(["com.test.new"]));
    let app = a.launch("com.test.new").unwrap();
    assert!(a.apps().iter().any(|x| x.pid == app.pid));
    a.terminate(app.pid).unwrap();
    assert!(a.apps().iter().all(|x| x.pid != app.pid));
}

#[test]
fn focus_theft_is_detected_and_strict_mode_restores() {
    let (a, m, _e, _i) = rig();
    m.with(|s| s.post_steals_focus = true);
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let o = ActOptions {
        max_tier: Tier::Targeted,
        ..opts()
    };
    let out = a.act(&s.id, None, Action::Keys("a".into()), &o).unwrap();
    assert_eq!(out.receipt.tier_used, Some(Tier::Targeted));
    assert!(
        out.receipt.focus_moved,
        "frontmost changed during a tier-3 act"
    );
    m.with(|s| s.front = OTHER_PID);
    let strict = ActOptions {
        strict_focus: true,
        ..o
    };
    let err = a
        .act(&s.id, None, Action::Keys("a".into()), &strict)
        .unwrap_err();
    assert_eq!(
        err,
        Error::FocusMoved {
            before: Some(OTHER_PID),
            after: Some(EDITOR_PID)
        }
    );
    assert_eq!(
        a.frontmost().unwrap().pid,
        OTHER_PID,
        "strict mode restores the original frontmost app"
    );
}

#[test]
fn type_text_prefers_selection_exact_native_write_with_readback() {
    let (a, m, _e, _i) = rig();
    m.set_area("hello world", (6, 5));
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let out = a
        .act(
            &s.id,
            Some(&id_of(&s, "Body")),
            Action::TypeText("there".into()),
            &opts(),
        )
        .unwrap();
    assert_eq!(out.receipt.tier_used, Some(Tier::Semantic));
    assert_eq!(m.area_text(), "hello there");
}

#[test]
fn acknowledged_but_unconfirmed_write_never_double_inserts() {
    let (a, m, _e, _i) = rig();
    m.with(|s| s.native_write_lies = true);
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let err = a
        .act(
            &s.id,
            Some(&id_of(&s, "Body")),
            Action::TypeText("x1".into()),
            &opts(),
        )
        .unwrap_err();
    assert!(matches!(err, Error::Platform(..)), "{err}");
    assert!(
        m.log().iter().all(|l| !l.starts_with("post:")),
        "must not climb after an acknowledged write"
    );
}

#[test]
fn type_text_falls_back_to_targeted_text_events() {
    let (a, m, _e, _i) = rig();
    m.with(|s| s.native_write_capable = false);
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let out = a
        .act(
            &s.id,
            Some(&id_of(&s, "Body")),
            Action::TypeText("abc".into()),
            &opts(),
        )
        .unwrap();
    assert_eq!(out.receipt.tier_used, Some(Tier::Targeted));
    assert_eq!(m.area_text(), "abc");
}

#[cfg(feature = "in-app")]
#[test]
fn keys_use_in_app_channel_when_available() {
    use rightkit_daemon::{
        host, Allowlist, CallContext, Emit, Handler, ServiceConfig, ServiceError, Suite,
    };
    use std::sync::Mutex;
    struct Rec(Mutex<Vec<(String, serde_json::Value)>>);
    impl Handler for Rec {
        fn call(
            &self,
            _: &CallContext,
            method: &str,
            params: &serde_json::Value,
            _: &Emit,
        ) -> std::result::Result<serde_json::Value, ServiceError> {
            self.0.lock().unwrap().push((method.into(), params.clone()));
            Ok(serde_json::json!({"ok": true}))
        }
    }
    // A real service host (user-only socket/pipe + service.json) the bridge reaches by discovery.
    let suite = Suite::per_run_temp("engine").unwrap();
    let rec = Arc::new(Rec(Mutex::new(vec![])));
    let hosted = host(
        &suite,
        ServiceConfig::new(
            "control-fake",
            "control:main",
            0,
            "test",
            Allowlist::new().allow("right-qa", ["key"]),
        ),
        rec.clone(),
    )
    .unwrap();
    let (a, m, _e, _i) = rig();
    a.add_in_app(Arc::new(
        in_app::ServiceBridge::new(EDITOR_PID, suite, "control-fake", "right-qa").unwrap(),
    ));
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let out = a
        .act(&s.id, None, Action::Keys("cmd+k".into()), &opts())
        .unwrap();
    assert_eq!(out.receipt.tier_used, Some(Tier::InApp));
    assert_eq!(
        rec.0.lock().unwrap().clone(),
        vec![("key".to_string(), serde_json::json!({"key": "cmd+k"}))]
    );
    assert!(m.log().iter().all(|l| !l.starts_with("post:")));
    hosted.shutdown();
}

#[test]
fn batch_stops_on_first_non_done_step() {
    let (a, m, _e, _i) = rig();
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    let steps = vec![
        (s.id.clone(), Some(id_of(&s, "Save")), Action::Press),
        (s.id.clone(), Some(id_of(&s, "Delete")), Action::Press), // needs confirmation
        (s.id.clone(), Some(id_of(&s, "Save")), Action::Press),
    ];
    let r = a.act_batch(&steps, &opts());
    assert_eq!(r.len(), 2);
    assert!(r[0].as_ref().unwrap().done());
    assert!(r[1].is_err());
    assert_eq!(m.with(|s| s.pressed.len()), 1);
}

#[test]
fn screenshot_writes_a_png() {
    let (a, _m, _e, _i) = rig();
    let p = std::env::temp_dir().join(format!("rk-auto-shot-{}.png", std::process::id()));
    let wid = a.screenshot(&Target::app(EDITOR_PID), &p).unwrap();
    assert_eq!(wid, 7);
    assert_eq!(
        png::dimensions(&std::fs::read(&p).unwrap()),
        Some((400, 300))
    );
    let _ = std::fs::remove_file(p);
}

#[test]
fn screenshots_mask_secure_and_credential_fields_and_hook_regions() {
    let (a, _m, _e, _i) = rig();
    let p = std::env::temp_dir().join(format!("rk-auto-mask-{}.png", std::process::id()));
    let rep = a.screenshot_report(&Target::app(EDITOR_PID), &p).unwrap();
    assert_eq!(
        rep.masked, 1,
        "the password field is masked without any hook"
    );
    let (w, _h, px) = png::decode_rgba(&std::fs::read(&p).unwrap()).unwrap();
    let at =
        |x: usize, y: usize| px[(y * w as usize + x) * 4..(y * w as usize + x) * 4 + 3].to_vec();
    assert_eq!(
        at(50, 20),
        vec![0, 0, 0],
        "inside the secure field's frame (10,10,80,20)"
    );
    assert_eq!(at(200, 200), vec![255, 255, 255], "elsewhere untouched");
    // A caller hook adds regions; the Body text is named by value.
    let s = a
        .snapshot(Target::app(EDITOR_PID), Limits::default())
        .unwrap();
    a.act(
        &s.id,
        Some(&id_of(&s, "Body")),
        Action::SetValue("account 12345".into()),
        &opts(),
    )
    .unwrap();
    a.add_redactor(Arc::new(MaskText(vec!["account 12345".into()])));
    let rep = a.screenshot_report(&Target::app(EDITOR_PID), &p).unwrap();
    assert!(rep.masked >= 2);
    // Fail closed: when masking is required and the capture cannot be rewritten, nothing is returned.
    assert!(a
        .screenshot(
            &Target::app(EDITOR_PID),
            std::path::Path::new("relative.png")
        )
        .is_err());
    let _ = std::fs::remove_file(p);
}

#[test]
fn a_replaced_editor_receives_text_only_when_provably_the_same_box() {
    let (a, m, _e, _i) = rig();
    m.with(|s| s.els[MockPlatform::AREA].text = Some("draft".into()));
    let p = pin(&a);
    assert!(a.replacement_pin(&p).is_none(), "the original still exists");
    // The app destroys and re-creates its composer, restoring the same text.
    m.with(|s| {
        let mut n = s.els[MockPlatform::AREA].clone();
        n.parent = Some(0);
        s.els[MockPlatform::AREA].alive = false;
        s.els.push(n);
        let i = s.els.len() - 1;
        s.els[0].children.push(i);
        s.focused = Some(i);
    });
    let r = a
        .replacement_pin(&p)
        .expect("same window, same text, original gone");
    let rec = a.deliver_text(&r, " more", &DeliveryOptions::default());
    assert!(rec.inserted(), "{:?}", rec.outcome);
    // A replacement that holds different text never receives.
    let (a, m, _e, _i) = rig();
    m.with(|s| s.els[MockPlatform::AREA].text = Some("draft".into()));
    let p = pin(&a);
    m.with(|s| {
        let mut n = s.els[MockPlatform::AREA].clone();
        n.text = Some("something else".into());
        s.els[MockPlatform::AREA].alive = false;
        s.els.push(n);
        let i = s.els.len() - 1;
        s.els[0].children.push(i);
        s.focused = Some(i);
    });
    assert!(a.replacement_pin(&p).is_none());
}

// ------------------------------------------------------------------ delivery

fn pin(a: &Automation<MockPlatform>) -> FocusPin<usize> {
    a.capture_pin(Some(EDITOR_PID)).unwrap()
}

#[test]
fn delivery_native_write_is_tier1_and_confirmed() {
    let (a, m, _e, _i) = rig();
    m.set_area("draft ", (6, 0));
    let r = a.deliver_text(&pin(&a), "hello", &DeliveryOptions::default());
    assert_eq!(
        r.outcome,
        DeliveryOutcome::Inserted {
            tier: Tier::Semantic
        },
        "{:?}",
        r.attempts
    );
    assert_eq!(m.area_text(), "draft hello");
    assert!(m
        .log()
        .iter()
        .all(|l| !l.starts_with("post:") && !l.starts_with("foreground:") && l != "clip_write"));
    assert_eq!(a.frontmost().unwrap().pid, OTHER_PID);
}

#[test]
fn delivery_falls_to_targeted_events_when_native_write_is_unavailable() {
    let (a, m, _e, _i) = rig();
    m.with(|s| s.native_write_capable = false);
    let r = a.deliver_text(&pin(&a), "hello", &DeliveryOptions::default());
    assert_eq!(
        r.outcome,
        DeliveryOutcome::Inserted {
            tier: Tier::Targeted
        }
    );
    assert_eq!(m.area_text(), "hello");
}

#[test]
fn unconfirmed_native_write_is_reported_and_not_retried() {
    let (a, m, _e, _i) = rig();
    m.with(|s| s.native_write_lies = true);
    let opts = DeliveryOptions {
        max_tier: Tier::Foreground,
        readback_ms: 100,
        ..Default::default()
    };
    let r = a.deliver_text(&pin(&a), "hello", &opts);
    assert!(
        matches!(
            r.outcome,
            DeliveryOutcome::SentUnconfirmed {
                tier: Tier::Semantic,
                ..
            }
        ),
        "{:?}",
        r.outcome
    );
    assert_eq!(m.area_text(), "");
    assert!(m
        .log()
        .iter()
        .all(|l| !l.starts_with("post:") && !l.starts_with("foreground:")));
}

#[test]
fn nothing_landed_escalates_to_foreground_paste_only_with_opt_in() {
    let (a, m, _e, _i) = rig();
    m.with(|s| {
        s.native_write_capable = false;
        s.post_lands = false;
    });
    let base = DeliveryOptions {
        readback_ms: 100,
        ..Default::default()
    };
    let r = a.deliver_text(&pin(&a), "hello", &base);
    assert!(
        matches!(r.outcome, DeliveryOutcome::NotDelivered(_)),
        "{:?}",
        r.outcome
    );
    assert!(m
        .log()
        .iter()
        .all(|l| !l.starts_with("foreground:") && l != "clip_write"));
    assert_eq!(m.with(|s| s.clip.clone()), Some("user clipboard".into()));

    let opt_in = DeliveryOptions {
        max_tier: Tier::Foreground,
        ..base
    };
    let r = a.deliver_text(&pin(&a), "hello", &opt_in);
    assert_eq!(
        r.outcome,
        DeliveryOutcome::Inserted {
            tier: Tier::Foreground
        },
        "{:?}",
        r.attempts
    );
    assert_eq!(m.area_text(), "hello");
    assert_eq!(
        m.with(|s| s.clip.clone()),
        Some("user clipboard".into()),
        "clipboard restored"
    );
    assert_eq!(
        a.frontmost().unwrap().pid,
        OTHER_PID,
        "frontmost restored after foreground paste"
    );
}

#[test]
fn garbled_delivery_is_unproven_and_stops() {
    let (a, m, _e, _i) = rig();
    m.with(|s| {
        s.native_write_capable = false;
        s.post_garbles = true;
    });
    let opts = DeliveryOptions {
        max_tier: Tier::Foreground,
        readback_ms: 100,
        ..Default::default()
    };
    let r = a.deliver_text(&pin(&a), "hello", &opts);
    assert!(
        matches!(
            r.outcome,
            DeliveryOutcome::SentUnconfirmed {
                tier: Tier::Targeted,
                ..
            }
        ),
        "{:?}",
        r.outcome
    );
    assert!(m.log().iter().all(|l| !l.starts_with("foreground:")));
}

#[test]
fn blind_editor_is_sent_unconfirmed_never_inserted() {
    let (a, m, _e, _i) = rig();
    m.with(|s| s.els[MockPlatform::AREA].blind = true);
    let r = a.deliver_text(&pin(&a), "hello", &DeliveryOptions::default());
    assert!(
        matches!(r.outcome, DeliveryOutcome::SentUnconfirmed { tier: Tier::Targeted, ref reason } if reason == "readback_unavailable"),
        "{:?}",
        r.outcome
    );
}

#[test]
fn secure_target_is_vetoed_even_with_fallback_copy_off() {
    let (a, m, _e, _i) = rig();
    m.with(|s| s.focused = Some(MockPlatform::PASSWORD));
    let p = pin(&a);
    let r = a.deliver_text(&p, "hunter2", &DeliveryOptions::default());
    assert_eq!(r.outcome, DeliveryOutcome::NotDelivered(Why::SecureTarget));
    assert_eq!(
        m.with(|s| s.els[MockPlatform::PASSWORD].text.clone().unwrap()),
        ""
    );
    assert_eq!(
        m.with(|s| s.clip.clone()),
        Some("user clipboard".into()),
        "clipboard untouched unless opted in"
    );
    // opt-in recovery copy
    let r = a.deliver_text(
        &p,
        "hunter2",
        &DeliveryOptions {
            fallback_copy: true,
            ..Default::default()
        },
    );
    assert_eq!(
        r.outcome,
        DeliveryOutcome::CopiedFallback(Why::SecureTarget)
    );
}

#[test]
fn focus_change_after_pin_refuses_delivery() {
    let (a, m, _e, _i) = rig();
    let p = pin(&a);
    m.with(|s| s.focused = Some(MockPlatform::PASSWORD));
    let r = a.deliver_text(&p, "hello", &DeliveryOptions::default());
    assert_eq!(r.outcome, DeliveryOutcome::NotDelivered(Why::FocusChanged));
    assert!(!a.pin_is_current(&p));
    // a recycled pid is a different process
    m.with(|s| {
        s.focused = Some(MockPlatform::AREA);
        s.apps[0].started = Some(1);
    });
    assert!(!a.pin_is_current(&p));
}

#[test]
fn lossy_clipboard_refuses_destructive_paste() {
    let (a, m, _e, _i) = rig();
    m.with(|s| {
        s.native_write_capable = false;
        s.post_lands = false;
        s.clip_lossless = false;
    });
    let opts = DeliveryOptions {
        max_tier: Tier::Foreground,
        readback_ms: 100,
        ..Default::default()
    };
    let r = a.deliver_text(&pin(&a), "hello", &opts);
    assert_eq!(
        r.outcome,
        DeliveryOutcome::NotDelivered(Why::ClipboardLossy)
    );
    assert_eq!(m.with(|s| s.clip.clone()), Some("user clipboard".into()));
}

#[test]
fn empty_text_and_missing_accessibility() {
    let (a, m, _e, _i) = rig();
    let p = pin(&a);
    assert_eq!(
        a.deliver_text(&p, "  ", &DeliveryOptions::default())
            .outcome,
        DeliveryOutcome::NotDelivered(Why::EmptyText)
    );
    m.with(|s| s.trusted = false);
    assert_eq!(
        a.deliver_text(&p, "x", &DeliveryOptions::default()).outcome,
        DeliveryOutcome::NotDelivered(Why::NotTrusted)
    );
}

#[test]
fn enter_is_sent_only_after_confirmed_insertion() {
    let (a, m, _e, _i) = rig();
    let r = a.deliver_text(
        &pin(&a),
        "hi",
        &DeliveryOptions {
            send_enter: true,
            ..Default::default()
        },
    );
    assert!(r.inserted() && r.enter_sent);
    assert_eq!(m.area_text(), "hi\n");
    // unconfirmed insertion never submits
    let (a, m, _e, _i) = rig();
    m.with(|s| s.native_write_lies = true);
    let r = a.deliver_text(
        &pin(&a),
        "hi",
        &DeliveryOptions {
            send_enter: true,
            readback_ms: 50,
            ..Default::default()
        },
    );
    assert!(!r.enter_sent && !r.inserted());
    assert_eq!(m.area_text(), "");
}

#[test]
fn multi_paragraph_text_is_confirmed_under_canonical_line_breaks() {
    let (a, m, _e, _i) = rig();
    let r = a.deliver_text(&pin(&a), "one.\n\ntwo.", &DeliveryOptions::default());
    assert!(r.inserted(), "{:?}", r.outcome);
    assert_eq!(m.area_text(), "one.\n\ntwo.");
}
