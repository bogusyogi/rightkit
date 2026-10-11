//! End-to-end on macOS against real apps (TextEdit, Calculator): launched in the
//! background, driven through the real AX / event / capture APIs, closed again.
//! Modelled on HeardRight's `e2e_engine.rs`: real processes, real targets, a
//! host without Accessibility or Screen Recording prints `SKIP e2e_mac: <reason>` and
//! returns; `RIGHTKIT_REQUIRE_ASSETS=1` turns that gap into a failure instead. Isolated
//! evidence directory, no audio.
//!
//! One test function on purpose: the kill switch is process-wide and TextEdit is
//! a singleton, so scenarios run in sequence and share one cleanup guard.
//!
//! Run: `rightkit cargo test -p rightkit-automation --test e2e_mac`
//! (the test host process needs Accessibility and Screen Recording; when a managed
//! broker spawns it without them, run the built test binary from a granted shell).

// `harness = false`: every host needs a `main`, so the macOS gate lives in a module.
#[cfg(target_os = "macos")]
mod mac {

    use rightkit_automation::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    fn sleep(ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }

    type Auto = Automation<NativePlatform>;

    fn snap(a: &Auto, pid: i32) -> Snapshot {
        a.snapshot(
            Target::app(pid),
            Limits {
                max_depth: 14,
                max_nodes: 900,
            },
        )
        .expect("snapshot")
    }
    fn find(s: &Snapshot, f: impl Fn(&Node) -> bool) -> Option<Node> {
        s.root.find(&f).cloned()
    }
    fn area(a: &Auto, pid: i32) -> (Snapshot, Node) {
        let s = snap(a, pid);
        let n = find(&s, |n| n.role == "textarea").expect("TextEdit text area");
        (s, n)
    }
    fn value_of(a: &Auto, pid: i32) -> String {
        let (_, n) = area(a, pid);
        n.value.unwrap_or_default()
    }
    fn elem() -> ActOptions {
        ActOptions {
            guard: Guard::Element,
            ..Default::default()
        }
    }

    /// Admission hook that records every request and can deny or cap by action.
    #[derive(Default)]
    struct Hook {
        deny_action: Mutex<Option<String>>,
        cap: Mutex<Option<Tier>>,
        approved: Mutex<Vec<(EffectKind, String)>>,
        settled: Mutex<Vec<(String, ResultKind)>>,
    }
    impl Admission for Hook {
        fn approve(&self, req: &EffectRequest) -> Admit {
            self.approved
                .lock()
                .unwrap()
                .push((req.kind, req.action.clone()));
            if self.deny_action.lock().unwrap().as_deref() == Some(req.action.as_str()) {
                return Admit::Deny(format!("policy forbids {}", req.action));
            }
            match *self.cap.lock().unwrap() {
                Some(t) if req.kind == EffectKind::Act => Admit::AllowUpTo(t),
                _ => Admit::Allow,
            }
        }
        fn settle(&self, req: &EffectRequest, out: &Settled) {
            self.settled
                .lock()
                .unwrap()
                .push((req.action.clone(), out.result));
        }
    }

    /// Closes what the test opened, even when an assertion fails.
    struct Cleanup {
        a: Arc<Auto>,
        pids: Mutex<Vec<i32>>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let confirm = ActOptions {
                confirm: vec![RiskClass::Close, RiskClass::Delete],
                guard: Guard::Off,
                ..Default::default()
            };
            for pid in self.pids.lock().unwrap().iter().copied() {
                for _ in 0..5 {
                    let t = self.a.terminate(pid);
                    if std::env::var("RK_DEBUG").is_ok() {
                        eprintln!("cleanup terminate({pid}) -> {t:?}");
                    }
                    sleep(700);
                    if !self.a.apps().iter().any(|x| x.pid == pid) {
                        break;
                    }
                    // A "save changes" sheet blocks quitting: discard (explicitly confirmed).
                    if let Ok(s) = self.a.snapshot(
                        Target::app(pid),
                        Limits {
                            max_depth: 14,
                            max_nodes: 900,
                        },
                    ) {
                        if std::env::var("RK_DEBUG").is_ok() {
                            fn names(n: &Node, out: &mut Vec<String>) {
                                if n.role == "button" {
                                    out.push(format!("{:?}/{:?}", n.name, n.description));
                                }
                                n.children.iter().for_each(|c| names(c, out));
                            }
                            let mut v = vec![];
                            names(&s.root, &mut v);
                            eprintln!("cleanup buttons: {v:?}");
                        }
                        if let Some(b) = find(&s, |n| {
                            n.role == "button"
                                && matches!(
                                    n.name.as_deref(),
                                    Some("Delete") | Some("Don’t Save") | Some("Don't Save")
                                )
                        }) {
                            let _ = self.a.act(&s.id, Some(&b.id), Action::Press, &confirm);
                            sleep(400);
                        }
                    }
                }
                self.a.end_session(pid);
            }
        }
    }

    /// Whether the test host has the grants this gate needs. A missing grant prints
    /// `SKIP e2e_mac: <reason>` and returns false, so a hosted runner without grants
    /// reports a skip. `RIGHTKIT_REQUIRE_ASSETS=1` fails the gate instead.
    fn assets_ready(status: &Status) -> bool {
        let mut missing = vec![];
        if !status.accessibility {
            missing.push("Accessibility");
        }
        if !status.screen_capture {
            missing.push("Screen Recording");
        }
        if missing.is_empty() {
            return true;
        }
        let reason = format!("{} not granted to the test host", missing.join(" and "));
        precondition_missing(&reason, &format!("{status:?}"))
    }

    /// Whether this host has a GUI session the gate can drive: a window-server frontmost
    /// app, and TextEdit installed. Headless or hosted runners without a logged-in GUI
    /// session fail this and skip instead of failing on the app launch.
    fn gui_ready(a: &Auto) -> bool {
        if a.frontmost().is_none() {
            return precondition_missing("no GUI session (no frontmost app to drive)", "");
        }
        if !std::path::Path::new("/System/Applications/TextEdit.app").exists() {
            return precondition_missing("TextEdit.app is not installed", "");
        }
        true
    }

    /// Reports an unmet precondition: panics under `RIGHTKIT_REQUIRE_ASSETS=1`, otherwise
    /// prints `SKIP e2e_mac: <reason>` and returns false so the caller returns.
    fn precondition_missing(reason: &str, detail: &str) -> bool {
        if std::env::var("RIGHTKIT_REQUIRE_ASSETS").as_deref() == Ok("1") {
            panic!("e2e_mac: {reason} (RIGHTKIT_REQUIRE_ASSETS=1): {detail}");
        }
        println!("SKIP e2e_mac: {reason}");
        false
    }

    /// `harness = false` (see Cargo.toml): NSWorkspace publishes the running-app list
    /// and frontmost app through the *main* run loop, which the libtest harness never
    /// services on its worker threads. A CLI or test must drive automation from the
    /// main thread (an app with a live main loop may use any thread).
    pub fn main() {
        reset_kill();
        let out = tempfile::tempdir().unwrap();
        let a: Arc<Auto> = Arc::new(native(Policy::new([
            "com.apple.TextEdit",
            "com.apple.calculator",
        ])));
        if !assets_ready(&a.status()) || !gui_ready(&a) {
            return;
        }

        let evidence_path = out.path().join("evidence.jsonl");
        a.add_evidence(Arc::new(JsonlEvidence::open(&evidence_path).unwrap()));
        let journal = Arc::new(MemoryJournal::default());
        a.add_journal(journal.clone());
        let hook = Arc::new(Hook::default());
        a.set_admission(hook.clone());
        let cleanup = Cleanup {
            a: a.clone(),
            pids: Mutex::new(vec![]),
        };

        let front0 = a.frontmost().map(|f| f.pid);
        let unchanged = |what: &str| {
            assert_eq!(
                a.frontmost().map(|f| f.pid),
                front0,
                "frontmost app changed: {what}"
            )
        };

        // ---- allowlist and fail-closed relative paths
        if let Some(finder) = a
            .apps()
            .into_iter()
            .find(|x| x.app_id == "com.apple.finder")
        {
            assert!(
                matches!(
                    a.snapshot(Target::app(finder.pid), Limits::default()),
                    Err(Error::Blocked(_))
                ),
                "Finder is not allowlisted"
            );
        }
        assert!(
            JsonlEvidence::open(std::path::Path::new("evidence-relative.jsonl")).is_err(),
            "evidence never lands relative to the cwd"
        );
        assert!(
            matches!(a.launch("com.apple.Safari"), Err(Error::Blocked(_))),
            "a non-allowlisted app is refused before launch"
        );

        // ---- TextEdit: background launch, tier 1, stable ids
        let te = a.launch("com.apple.TextEdit").expect("launch TextEdit");
        cleanup.pids.lock().unwrap().push(te.pid);
        sleep(1500);
        unchanged("TextEdit launch");
        a.begin_session(te.pid, "e2e_mac").unwrap();
        // The relative-path refusal needs an allowlisted, running target. pid 1 is not a
        // running GUI app, so it would be refused earlier with NotFound and prove nothing here.
        assert!(
            matches!(
                a.screenshot(&Target::app(te.pid), std::path::Path::new("shot.png")),
                Err(Error::Invalid(_))
            ),
            "a cwd-relative screenshot path is refused before any capture"
        );
        let mut s = snap(&a, te.pid);
        if find(&s, |n| n.role == "textarea").is_none() {
            a.act(
                &s.id,
                None,
                Action::MenuPress(vec!["File".into(), "New".into()]),
                &ActOptions::default(),
            )
            .expect("File>New");
            sleep(900);
            s = snap(&a, te.pid);
        }
        let area_id = find(&s, |n| n.role == "textarea").unwrap().id;
        let s2 = snap(&a, te.pid);
        assert_eq!(
            s.fingerprint, s2.fingerprint,
            "tree structure is stable between reads"
        );
        assert_eq!(
            find(&s2, |n| n.role == "textarea").unwrap().id,
            area_id,
            "element ids are stable"
        );

        let o = a
            .act(
                &s2.id,
                Some(&area_id),
                Action::SetValue("tier1 value".into()),
                &elem(),
            )
            .expect("set value");
        assert_eq!(o.receipt.tier_used, Some(Tier::Semantic));
        sleep(150);
        assert_eq!(value_of(&a, te.pid), "tier1 value");
        assert!(!o.receipt.focus_moved);
        unchanged("tier 1 set value");

        // ---- lease: epoch fencing and acked input sequence (PTY-002 shape)
        let l1 = a
            .acquire_lease(te.pid, LeaseRequest::new("controller-a"))
            .unwrap();
        let (s, ar) = area(&a, te.pid);
        let unfenced = a
            .act(
                &s.id,
                Some(&ar.id),
                Action::SetValue("unfenced".into()),
                &elem(),
            )
            .unwrap_err();
        assert!(
            matches!(&unfenced, Error::Blocked(m) if m.contains("leased")),
            "{unfenced}"
        );
        let f = |seq: u64, epoch: u64| ActOptions {
            fence: Some(Fence { epoch, seq }),
            ..elem()
        };
        a.act(
            &s.id,
            Some(&ar.id),
            Action::SetValue("one".into()),
            &f(1, l1.epoch),
        )
        .unwrap();
        sleep(120);
        assert_eq!(value_of(&a, te.pid), "one");
        assert!(matches!(
            a.act(
                &s.id,
                Some(&ar.id),
                Action::SetValue("replay".into()),
                &f(1, l1.epoch)
            ),
            Err(Error::Replayed { acked_seq: 1 })
        ));
        assert_eq!(
            value_of(&a, te.pid),
            "one",
            "a replayed sequence is acknowledged but never executed again"
        );
        assert!(matches!(
            a.act(
                &s.id,
                Some(&ar.id),
                Action::SetValue("gap".into()),
                &f(3, l1.epoch)
            ),
            Err(Error::OutOfOrder {
                expected: 2,
                got: 3
            })
        ));
        assert_eq!(value_of(&a, te.pid), "one");
        a.act(
            &s.id,
            Some(&ar.id),
            Action::SetValue("two".into()),
            &f(2, l1.epoch),
        )
        .unwrap();
        sleep(120);
        assert_eq!(a.lease_status(te.pid).unwrap().acked_seq, 2);
        assert!(
            matches!(
                a.acquire_lease(te.pid, LeaseRequest::new("controller-b")),
                Err(Error::LeaseHeld { .. })
            ),
            "no silent takeover"
        );
        let l2 = a
            .acquire_lease(te.pid, LeaseRequest::new("controller-b").takeover())
            .unwrap();
        assert!(l2.epoch > l1.epoch);
        let stale = a
            .act(
                &s.id,
                Some(&ar.id),
                Action::SetValue("stale".into()),
                &f(3, l1.epoch),
            )
            .unwrap_err();
        assert!(
            matches!(stale, Error::StaleLease { presented, current: Some(c) } if presented == l1.epoch && c == l2.epoch),
            "{stale}"
        );
        assert_eq!(
            value_of(&a, te.pid),
            "two",
            "the displaced controller cannot inject"
        );
        a.act(
            &s.id,
            Some(&ar.id),
            Action::SetValue("three".into()),
            &f(1, l2.epoch),
        )
        .unwrap();
        sleep(120);
        assert_eq!(
            value_of(&a, te.pid),
            "three",
            "the new epoch restarts the sequence at 1"
        );
        assert!(
            matches!(a.terminate(te.pid), Err(Error::Blocked(_))),
            "a leased app cannot be terminated by someone without the lease"
        );
        assert!(a.release_lease(&l2));
        assert!(a.lease_status(te.pid).is_none());
        unchanged("lease scenarios");

        // ---- admission hook (EFF-001): deny before any side effect, cap the tier, settle
        let (s, ar) = area(&a, te.pid);
        *hook.deny_action.lock().unwrap() = Some("keys".into());
        let denied = a
            .act(
                &s.id,
                Some(&ar.id),
                Action::Keys("x".into()),
                &ActOptions {
                    guard: Guard::Off,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(denied, Error::Denied(_)), "{denied}");
        *hook.deny_action.lock().unwrap() = None;
        sleep(150);
        assert_eq!(
            value_of(&a, te.pid),
            "three",
            "a denied input had no side effect"
        );
        *hook.cap.lock().unwrap() = Some(Tier::Semantic);
        let capped = a
            .act(
                &s.id,
                Some(&ar.id),
                Action::Keys("x".into()),
                &ActOptions {
                    guard: Guard::Off,
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(
            matches!(capped, Error::NoTier(_)),
            "admission capped the ladder at tier 1: {capped}"
        );
        *hook.cap.lock().unwrap() = None;
        assert!(hook
            .settled
            .lock()
            .unwrap()
            .iter()
            .any(|(act, r)| act == "set_value" && *r == ResultKind::Done));
        assert!(hook
            .approved
            .lock()
            .unwrap()
            .iter()
            .any(|(k, act)| *k == EffectKind::Observe && act == "snapshot"));

        // ---- tier 3 targeted keys, frontmost untouched; evidence names the tier
        let o = a
            .act(
                &s.id,
                Some(&ar.id),
                Action::Keys("h i".into()),
                &ActOptions {
                    guard: Guard::Off,
                    ..Default::default()
                },
            )
            .expect("tier 3 keys");
        assert_eq!(o.receipt.tier_used, Some(Tier::Targeted));
        sleep(200);
        assert_eq!(value_of(&a, te.pid), "threehi");
        assert!(!o.receipt.focus_moved);
        unchanged("tier 3 keys");

        // ---- credentials are never typed
        let (s, ar) = area(&a, te.pid);
        let secret = format!("{}{}", "sk", "-abcdefghijklmnopqrstuvwxyz0123");
        let e = a
            .act(
                &s.id,
                Some(&ar.id),
                Action::TypeText(secret.clone()),
                &elem(),
            )
            .unwrap_err();
        assert!(
            matches!(&e, Error::Blocked(m) if m.contains("credential")),
            "{e}"
        );
        assert_eq!(
            value_of(&a, te.pid),
            "threehi",
            "the credential never reached the app"
        );
        let pin = a.capture_pin(Some(te.pid)).unwrap();
        let rec = a.deliver_text(&pin, &secret, &DeliveryOptions::default());
        assert!(
            matches!(rec.outcome, DeliveryOutcome::NotDelivered(Why::Blocked(_))),
            "{:?}",
            rec.outcome
        );
        assert_eq!(value_of(&a, te.pid), "threehi");

        // ---- delivery: pinned editor, read-back proof, canonical line breaks (spec 5.1 rule 5)
        let rec = a.deliver_text(&pin, "\nline one\n\nline two", &DeliveryOptions::default());
        sleep(150);
        assert!(rec.inserted(), "{:?}", rec.outcome);
        assert!(
            value_of(&a, te.pid).ends_with("line one\n\nline two"),
            "{:?}",
            value_of(&a, te.pid)
        );
        unchanged("delivery");

        // ---- rule 1/3: a pin never follows focus to another editor
        // File>New in a background TextEdit can take a moment (or need a second try) to open and focus a window.
        for attempt in 0..3 {
            let s = snap(&a, te.pid);
            a.act(
                &s.id,
                None,
                Action::MenuPress(vec!["File".into(), "New".into()]),
                &ActOptions::default(),
            )
            .expect("second document");
            sleep(900 + 600 * attempt);
            if !a.pin_is_current(&pin) {
                break;
            }
        }
        assert!(!a.pin_is_current(&pin), "focus moved to the new document");
        assert!(
            a.replacement_pin(&pin).is_none(),
            "rule 3: the original editor still exists, so no other editor may stand in for it"
        );
        let rec = a.deliver_text(&pin, "must not land anywhere", &DeliveryOptions::default());
        assert_eq!(
            rec.outcome,
            DeliveryOutcome::NotDelivered(Why::FocusChanged)
        );
        sleep(150);
        assert!(
            !value_of(&a, te.pid).contains("must not land"),
            "the new document is untouched"
        );
        unchanged("second document");

        // ---- screenshot redaction for QA evidence
        let (s, ar) = area(&a, te.pid);
        a.act(
            &s.id,
            Some(&ar.id),
            Action::SetValue("visible MARKER_ZEBRA tail".into()),
            &elem(),
        )
        .unwrap();
        sleep(250);
        let plain = out.path().join("plain.png");
        let rep = a
            .screenshot_report(&Target::app(te.pid), &plain)
            .expect("screenshot");
        assert_eq!(rep.masked, 0, "nothing sensitive on screen, nothing masked");
        let (w, h, px_plain) = png::decode_rgba(&std::fs::read(&plain).unwrap())
            .expect("real macOS PNG decodes (dynamic Huffman)");
        assert_eq!((w, h), (rep.width, rep.height));

        a.add_redactor(Arc::new(MaskText(vec!["MARKER_ZEBRA".into()])));
        let masked_path = out.path().join("masked.png");
        let rep = a
            .screenshot_report(&Target::app(te.pid), &masked_path)
            .expect("masked screenshot");
        assert!(rep.masked >= 1, "the hook's region was masked");
        let (w2, h2, px_masked) = png::decode_rgba(&std::fs::read(&masked_path).unwrap()).unwrap();
        assert_eq!((w2, h2), (w, h));
        let (_, ar) = area(&a, te.pid);
        let fr = ar.frame.expect("text area frame");
        let win = a
            .windows(false)
            .into_iter()
            .filter(|x| x.pid == te.pid && x.layer == 0)
            .max_by_key(|x| x.on_screen as u8)
            .unwrap()
            .bounds;
        let (sx, sy) = (w as f64 / win.w, h as f64 / win.h);
        let (cx, cy) = (
            ((fr.x + fr.w / 2.0 - win.x) * sx) as usize,
            ((fr.y + fr.h / 2.0 - win.y) * sy) as usize,
        );
        let at = |p: &[u8], x: usize, y: usize| {
            p[(y * w as usize + x) * 4..(y * w as usize + x) * 4 + 3].to_vec()
        };
        assert_eq!(
            at(&px_masked, cx, cy),
            vec![0, 0, 0],
            "centre of the masked region is solid black"
        );
        assert_ne!(
            at(&px_plain, cx, cy),
            vec![0, 0, 0],
            "the unmasked capture shows the real text area (white)"
        );
        assert_eq!(
            at(&px_masked, 4, 4),
            at(&px_plain, 4, 4),
            "pixels outside the region are untouched"
        );
        unchanged("screenshots");

        // ---- Calculator: tier 1 only, then a crash is reported
        let calc = a.launch("com.apple.calculator").expect("launch Calculator");
        cleanup.pids.lock().unwrap().push(calc.pid);
        sleep(1500);
        a.begin_session(calc.pid, "e2e_mac").unwrap();
        let press = |name: &str| {
            let s = snap(&a, calc.pid);
            let id = find(&s, |n| {
                n.role == "button"
                    && (n.description.as_deref() == Some(name)
                        || n.name.as_deref() == Some(name)
                        || n.identifier.as_deref().is_some_and(|i| i.ends_with(name)))
            })
            .map(|n| n.id);
            let id = id.unwrap_or_else(|| panic!("no Calculator button {name}"));
            a.act(
                &s.id,
                Some(&id),
                Action::Press,
                &ActOptions {
                    guard: Guard::Off,
                    ..Default::default()
                },
            )
            .expect("press")
        };
        for key in ["All Clear", "7", "Add", "5", "Equals"] {
            let o = if key == "All Clear" {
                // "All Clear" is "Clear" when the display is already clear.
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| press("All Clear")))
                    .unwrap_or_else(|_| press("Clear"))
            } else {
                press(key)
            };
            assert_eq!(o.receipt.tier_used, Some(Tier::Semantic), "{key}");
        }
        sleep(400);
        let s = snap(&a, calc.pid);
        let mut texts = vec![];
        fn walk(n: &Node, out: &mut Vec<String>) {
            if n.role == "text" {
                out.extend(n.value.clone());
            }
            n.children.iter().for_each(|c| walk(c, out));
        }
        walk(&s.root, &mut texts);
        assert!(
            texts.iter().any(|t| t.contains("12")),
            "7 + 5 = 12 with AXPress only: {texts:?}"
        );
        unchanged("calculator");

        // The target crashes (SIGKILL by someone else): the journal says so, the session ends.
        let killed_ok = std::process::Command::new("kill")
            .args(["-9", &calc.pid.to_string()])
            .status()
            .unwrap()
            .success();
        assert!(killed_ok);
        sleep(800);
        let gone = a.poll_targets();
        assert!(
            gone.contains(&calc.pid),
            "poll_targets reports the vanished app: {gone:?}"
        );
        assert!(journal
            .all()
            .iter()
            .any(|e| matches!(e, LifecycleEvent::TargetExited { pid, .. } if *pid == calc.pid)));

        // ---- kill switch fails closed mid-session
        let s = snap(&a, te.pid);
        kill();
        let blocked = a.act(
            &s.id,
            None,
            Action::MenuPress(vec!["View".into()]),
            &ActOptions::default(),
        );
        reset_kill();
        assert!(matches!(blocked, Err(Error::Blocked(_))));

        // ---- journal and metrics: content-free
        let events = journal.all();
        for kind in [
            "started",
            "lease_granted",
            "lease_fenced",
            "denied",
            "target_exited",
        ] {
            assert!(
                events.iter().any(|e| e.kind() == kind),
                "journal lacks {kind}: {:?}",
                events.iter().map(|e| e.kind()).collect::<Vec<_>>()
            );
        }
        assert!(
            events
                .iter()
                .all(|e| !e.to_json().contains("abcdefghij")
                    && !e.to_json().contains("MARKER_ZEBRA")),
            "no typed text in lifecycle events"
        );
        let m = a.metrics();
        assert!(
            m.acts >= 15
                && m.denied >= 5
                && m.replayed == 1
                && m.fenced >= 2
                && m.by_tier[0] > 0
                && m.by_tier[2] > 0,
            "{m:?}"
        );
        assert_eq!(m.screenshots, 2);
        let evidence = std::fs::read_to_string(&evidence_path).unwrap();
        assert!(
            evidence.contains("\"tier_used\":\"semantic\"")
                && evidence.contains("\"tier_used\":\"targeted\""),
            "per-action tier evidence"
        );
        assert!(
            evidence.contains("\"lease_epoch\":"),
            "receipts carry the lease fence"
        );
        assert!(
            !evidence.contains("abcdefghij"),
            "no credential in any receipt"
        );

        // ---- close what we opened
        drop(cleanup);
        sleep(500);
        let running: Vec<i32> = a.apps().into_iter().map(|x| x.pid).collect();
        assert!(
            !running.contains(&te.pid) && !running.contains(&calc.pid),
            "closed what it opened"
        );
        assert_eq!(
            a.frontmost().map(|f| f.pid),
            front0,
            "frontmost unchanged at the end"
        );
        let m = a.metrics();
        println!("e2e_mac OK: {} acts, {} denied, {} replayed, {} fenced, tiers {:?}, {} screenshots ({} regions masked)", m.acts, m.denied, m.replayed, m.fenced, m.by_tier, m.screenshots, m.screenshot_regions_masked);
    }
}

#[cfg(target_os = "macos")]
fn main() {
    mac::main()
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("e2e_mac: macOS-only end-to-end gate; nothing to run on this host");
}
