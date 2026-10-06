//! macOS proof against harmless targets (TextEdit, Calculator): drives them in
//! the background, checks the frontmost app never changes, records which tier ran,
//! then closes everything it opened.
//!
//! Run: rightkit cargo run -p rightkit-automation --example mac_proof
//! Env: RKA_OUT=<dir> for evidence (default: a temp dir)

use rightkit_automation::*;
use std::sync::Arc;
use std::time::Duration;

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

struct Run {
    checks: Vec<(String, bool, String)>,
}
impl Run {
    /// A finding that is recorded but is not a pass/fail gate (known platform limits).
    fn note(&mut self, name: &str, detail: impl Into<String>) {
        let detail = detail.into();
        println!("[note] {name}: {detail}");
        self.checks.push((format!("note: {name}"), true, detail));
    }
    fn check(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        let detail = detail.into();
        println!("[{}] {name}: {detail}", if ok { "ok" } else { "FAIL" });
        self.checks.push((name.into(), ok, detail));
    }
}

fn find<'a>(s: &'a Snapshot, f: impl Fn(&Node) -> bool) -> Option<&'a Node> {
    s.root.find(&f)
}

fn snap(a: &Automation<NativePlatform>, pid: i32) -> Snapshot {
    a.snapshot(Target::app(pid), Limits { max_depth: 14, max_nodes: 900 }).expect("snapshot")
}

fn main() {
    let out = std::env::var("RKA_OUT").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir().join("rka-proof"));
    std::fs::create_dir_all(&out).unwrap();
    let a = native(Policy::new(["com.apple.TextEdit", "com.apple.calculator"]));
    a.add_evidence(Arc::new(JsonlEvidence::open(&out.join("evidence.jsonl")).unwrap()));
    let ind = Arc::new(MemoryIndicator::default());
    a.set_indicator(ind.clone());
    let mut r = Run { checks: vec![] };

    let st = a.status();
    println!("status: {st:?}");
    if !st.accessibility {
        eprintln!("Accessibility not granted to this host process");
        std::process::exit(2);
    }
    let front0 = a.frontmost().map(|f| f.pid);
    println!("frontmost before: {front0:?}");
    let same_front = |a: &Automation<NativePlatform>| a.frontmost().map(|f| f.pid) == front0;

    // Safety first: Finder is not allowlisted; the kill switch fails closed.
    if let Some(finder) = a.apps().into_iter().find(|x| x.app_id == "com.apple.finder") {
        r.check("allowlist denies Finder", matches!(a.snapshot(Target::app(finder.pid), Limits::default()), Err(Error::Blocked(_))), "Blocked");
    }

    // ------------------------------------------------------------- TextEdit
    let te = a.launch("com.apple.TextEdit").expect("launch TextEdit");
    sleep(1500);
    r.check("TextEdit launched in background", same_front(&a), format!("pid {} front still {:?}", te.pid, a.frontmost().map(|f| f.pid)));
    a.begin_session(te.pid, "mac_proof").unwrap();
    let mut s = snap(&a, te.pid);
    if find(&s, |n| n.role == "textarea").is_none() {
        let o = a.act(&s.id, None, Action::MenuPress(vec!["File".into(), "New".into()]), &ActOptions::default()).expect("File>New");
        r.check("menu File>New", o.receipt.tier_used == Some(Tier::Semantic), format!("tier {:?}", o.receipt.tier_used));
        sleep(900);
        s = snap(&a, te.pid);
    }
    let area = find(&s, |n| n.role == "textarea").expect("text area").id.clone();
    let mut o = a.act(&s.id, Some(&area), Action::SetValue("tier1 set-value".into()), &ActOptions::default()).expect("set value");
    if let Some(fresh) = o.fresh.clone() {
        // The freshness guard saw the window still settling and refused to act on the stale snapshot.
        r.note("freshness guard returned a fresh snapshot instead of acting", format!("{} nodes", fresh.root.count()));
        o = a.act(&fresh.id, Some(&area), Action::SetValue("tier1 set-value".into()), &ActOptions::default()).expect("set value");
    }
    sleep(150);
    let s = snap(&a, te.pid);
    let v = find(&s, |n| n.id == area).and_then(|n| n.value.clone()).unwrap_or_default();
    r.check("tier1 AXValue set + read back", o.receipt.tier_used == Some(Tier::Semantic) && v == "tier1 set-value", format!("tier {:?} {v:?}", o.receipt.tier_used));
    r.check("frontmost unchanged after set-value", same_front(&a) && !o.receipt.focus_moved, "");

    // Tier 3: pid-addressed key events (plain keys and text-system chords), no focus change.
    let o = a.act(&s.id, Some(&area), Action::Keys("h i".into()), &ActOptions { guard: Guard::Off, ..Default::default() }).expect("keys");
    sleep(200);
    let s = snap(&a, te.pid);
    let v = find(&s, |n| n.id == area).and_then(|n| n.value.clone()).unwrap_or_default();
    r.check("tier3 key events typed text", o.receipt.tier_used == Some(Tier::Targeted) && v == "tier1 set-valuehi", format!("tier {:?} value {v:?}", o.receipt.tier_used));
    let o = a.act(&s.id, Some(&area), Action::Keys("backspace backspace".into()), &ActOptions { guard: Guard::Off, ..Default::default() }).expect("keys");
    sleep(200);
    let s = snap(&a, te.pid);
    let v = find(&s, |n| n.id == area).and_then(|n| n.value.clone()).unwrap_or_default();
    r.check("tier3 backspace chords edited text", o.receipt.tier_used == Some(Tier::Targeted) && v == "tier1 set-value", format!("value {v:?}"));
    r.check("frontmost unchanged after tier3 keys", same_front(&a) && !o.receipt.focus_moved, "");
    // Command-key shortcuts are menu key equivalents: probe whether a background app honours them at tier 3.
    let _ = a.act(&s.id, Some(&area), Action::Keys("cmd+a".into()), &ActOptions { guard: Guard::Off, ..Default::default() });
    sleep(200);
    let sel = a.platform().focused(te.pid).ok().flatten().and_then(|e| a.platform().field_text(&e)).and_then(|f| f.selection);
    r.note("tier3 cmd+a (menu key equivalent) in a background app", format!("selection after: {sel:?} (len 15 = honoured; 0 = ignored, use menu_press or foreground)"));
    r.note("menu Edit>Select All via AXPress in a background app", "needs a key window; use foreground tier or AXSelectedTextRange (documented limit)");
    // Tier 3 pointer into an AppKit text view: does a background click move the caret?
    if let Some(fr) = find(&s, |n| n.id == area).and_then(|n| n.frame) {
        let _ = a.act(&s.id, Some(&area), Action::Click { point: Some((fr.x + 6.0, fr.y + 8.0)), button: Button::Left, count: 1 }, &ActOptions { min_tier: Tier::Targeted, guard: Guard::Off, ..Default::default() });
        sleep(250);
        let sel = a.platform().focused(te.pid).ok().flatten().and_then(|e| a.platform().field_text(&e)).and_then(|f| f.selection);
        r.note("tier3 per-pid mouse click into a background AppKit text view", format!("caret after click: {sel:?} (start of text = delivered; (15,0) = ignored)"));
        r.check("frontmost unchanged after tier3 click into TextEdit", same_front(&a), "");
    }
    // Tier 4 (explicit opt-in): foreground chord, then focus must come back. If the OS refuses
    // activation (a system alert can hold the front), the engine must fail closed: no event posted.
    let before_log = std::fs::read_to_string(out.join("evidence.jsonl")).unwrap_or_default().lines().count();
    let t4 = a.act(&s.id, Some(&area), Action::Keys("right".into()), &ActOptions { min_tier: Tier::Foreground, max_tier: Tier::Foreground, guard: Guard::Off, ..Default::default() });
    sleep(200);
    let sel = a.platform().focused(te.pid).ok().flatten().and_then(|e| a.platform().field_text(&e)).and_then(|f| f.selection);
    match &t4 {
        Ok(o) => r.check("tier4 foreground key moved the caret, then focus restored", o.receipt.tier_used == Some(Tier::Foreground) && sel == Some((15, 0)) && same_front(&a), format!("selection {sel:?} front {:?}", a.frontmost().map(|f| f.pid))),
        Err(e) => {
            let front = a.frontmost();
            r.check("tier4 refused activation fails closed (no HID events posted, focus untouched)", sel == Some((15, 0)) && same_front(&a), format!("{e}; front {:?}", front.map(|f| f.app_id)));
            r.note("tier4 not provable in this run", format!("OS refused to activate the target ({e}); lines logged {}", std::fs::read_to_string(out.join("evidence.jsonl")).unwrap_or_default().lines().count() - before_log));
        }
    }

    // Delivery with HeardRight semantics: pin, native selection write, read-back proof.
    let pin = a.capture_pin(Some(te.pid)).expect("pin");
    let rec = a.deliver_text(&pin, " + delivered", &DeliveryOptions::default());
    sleep(150);
    let v = a.focused(te.pid).ok().flatten().and_then(|n| n.value).unwrap_or_default();
    r.check("deliver_text tier1 confirmed by read-back", rec.outcome == DeliveryOutcome::Inserted { tier: Tier::Semantic } && v.ends_with("+ delivered"), format!("{:?} value {v:?}", rec.outcome));
    r.check("frontmost unchanged after delivery", same_front(&a), "");
    // Delivery over targeted events only (tier 1 disabled by min tier through a pin on a blind-free field).
    let o = a.act(&s.id, Some(&area), Action::TypeText(" +T3".into()), &ActOptions { min_tier: Tier::Targeted, guard: Guard::Off, ..Default::default() }).expect("type t3");
    sleep(200);
    let v = a.focused(te.pid).ok().flatten().and_then(|n| n.value).unwrap_or_default();
    r.check("tier3 unicode text events", o.receipt.tier_used == Some(Tier::Targeted) && v.ends_with("+T3"), format!("{v:?}"));

    // Observe: focused element, screenshot.
    let f = a.focused(te.pid).unwrap();
    r.check("focused element is the text area", f.as_ref().map(|n| n.role.as_str()) == Some("textarea"), format!("{:?}", f.map(|n| n.native_role)));
    let shot = out.join("textedit.png");
    match a.screenshot(&Target::app(te.pid), &shot) {
        Ok(wid) => {
            let d = png_dims(&shot);
            r.check("per-window screenshot while TextEdit is in the background", d.is_some(), format!("window {wid} {d:?}"));
        }
        Err(e) => r.check("per-window screenshot", false, e.to_string()),
    }
    r.check("TextEdit still background", same_front(&a), "");

    // ------------------------------------------------------------ Calculator
    let calc = a.launch("com.apple.calculator").expect("launch Calculator");
    sleep(1500);
    a.begin_session(calc.pid, "mac_proof").unwrap();
    let s = snap(&a, calc.pid);
    let btn = |s: &Snapshot, name: &str| find(s, |n| n.role == "button" && (n.description.as_deref() == Some(name) || n.name.as_deref() == Some(name) || n.identifier.as_deref().is_some_and(|i| i.ends_with(name)))).map(|n| n.id.clone());
    let display = |s: &Snapshot| -> String {
        let mut all = vec![];
        fn walk(n: &Node, out: &mut Vec<String>) {
            if n.role == "text" {
                if let Some(v) = &n.value {
                    out.push(v.clone());
                }
            }
            n.children.iter().for_each(|c| walk(c, out));
        }
        walk(&s.root, &mut all);
        all.join("|")
    };
    println!("calculator display texts: {}", display(&s));
    let press = |a: &Automation<NativePlatform>, name: &str, min: Tier| -> Option<ActOutcome> {
        let s = snap(a, calc.pid);
        let id = btn(&s, name)?;
        a.act(&s.id, Some(&id), Action::Click { point: None, button: Button::Left, count: 1 }, &ActOptions { min_tier: min, guard: Guard::Off, ..Default::default() }).ok()
    };
    let clear = press(&a, "All Clear", Tier::Semantic).or_else(|| press(&a, "Clear", Tier::Semantic));
    r.check("Calculator tier1 AXPress", clear.as_ref().map(|o| o.receipt.tier_used) == Some(Some(Tier::Semantic)), format!("{:?}", clear.map(|o| o.receipt.tier_used)));
    let seven = press(&a, "7", Tier::Semantic);
    let plus = press(&a, "Add", Tier::Semantic).or_else(|| press(&a, "+", Tier::Semantic));
    let five = press(&a, "5", Tier::Semantic);
    let equals = press(&a, "Equals", Tier::Semantic).or_else(|| press(&a, "=", Tier::Semantic));
    sleep(400);
    let s = snap(&a, calc.pid);
    let d = display(&s);
    r.check("Calculator computed 7+5=12 with tier1 AXPress only", seven.is_some() && plus.is_some() && five.is_some() && equals.is_some() && d.contains("12"), format!("display: {d}"));
    // Tier 3 pointer events: record what this macOS does with them (known to be dropped for background windows).
    let t3 = press(&a, "9", Tier::Targeted);
    sleep(300);
    let d3 = display(&snap(&a, calc.pid));
    r.note("tier3 per-pid mouse click on background Calculator", format!("tier {:?}; display {d3} ({})", t3.as_ref().map(|o| o.receipt.tier_used), if d3.contains('9') { "delivered" } else { "ignored by the background app, as documented" }));
    r.check("frontmost unchanged after tier3 pointer events", same_front(&a), "");
    // Tier 4 (explicit opt-in): foreground click works, then the previous app is restored.
    let t4 = press(&a, "All Clear", Tier::Foreground).or_else(|| press(&a, "Clear", Tier::Foreground));
    sleep(400);
    let d4 = display(&snap(&a, calc.pid));
    match t4 {
        Some(o) => r.check("tier4 foreground click cleared the display and focus was restored", o.receipt.tier_used == Some(Tier::Foreground) && !d4.contains("12") && same_front(&a), format!("display: {d4}; front {:?}", a.frontmost().map(|f| f.pid))),
        None => r.check("tier4 refused activation fails closed (display untouched, focus untouched)", d4.contains("12") && same_front(&a), format!("display: {d4}")),
    }
    let shot = out.join("calculator.png");
    r.check("Calculator screenshot", a.screenshot(&Target::app(calc.pid), &shot).is_ok() && png_dims(&shot).is_some(), format!("{:?}", png_dims(&shot)));

    // ------------------------------------------------------------ kill switch
    let s2 = snap(&a, calc.pid);
    kill();
    let blocked = matches!(a.act(&s2.id, None, Action::MenuPress(vec!["View".into()]), &ActOptions::default()), Err(Error::Blocked(_)));
    reset_kill();
    r.check("kill switch fails closed mid-session", blocked, "");

    // ------------------------------------------------------------ cleanup
    a.end_session(calc.pid);
    let _ = a.terminate(calc.pid);
    sleep(800);
    // TextEdit: quit this instance; if a "save changes" sheet blocks it, discard (explicitly confirmed).
    let confirm = ActOptions { confirm: vec![RiskClass::Close, RiskClass::Delete], guard: Guard::Off, ..Default::default() };
    for _ in 0..4 {
        let _ = a.terminate(te.pid);
        sleep(800);
        if !a.apps().iter().any(|x| x.pid == te.pid) {
            break;
        }
        if let Ok(s) = a.snapshot(Target::app(te.pid), Limits { max_depth: 14, max_nodes: 900 }) {
            if let Some(b) = find(&s, |n| n.role == "button" && matches!(n.name.as_deref(), Some("Delete") | Some("Don’t Save") | Some("Don't Save"))) {
                let _ = a.act(&s.id, Some(&b.id.clone()), Action::Press, &confirm);
                sleep(400);
            }
        }
    }
    a.end_session(te.pid);
    let running: Vec<i32> = a.apps().into_iter().map(|x| x.pid).collect();
    r.check("closed what it opened", !running.contains(&te.pid) && !running.contains(&calc.pid), format!("TextEdit {} Calculator {}", te.pid, calc.pid));
    r.check("frontmost unchanged at the end", same_front(&a), format!("{:?} -> {:?}", front0, a.frontmost().map(|f| f.pid)));

    let events = ind.all();
    r.check("indicator events emitted", events.iter().any(|e| matches!(e, SessionEvent::Started { .. })) && events.iter().any(|e| matches!(e, SessionEvent::Acted { .. })) && events.iter().any(|e| matches!(e, SessionEvent::Ended { .. })), format!("{} events", events.len()));

    let failed = r.checks.iter().filter(|c| !c.1).count();
    let items: Vec<String> = r.checks.iter().map(|c| format!("{{\"check\":{},\"ok\":{},\"detail\":{}}}", json::s(&c.0), c.1, json::s(&c.2))).collect();
    std::fs::write(out.join("proof.json"), format!("{{\"failed\":{failed},\"checks\":[{}]}}", items.join(","))).unwrap();
    println!("evidence dir: {}", out.display());
    if failed == 0 {
        println!("PROOF OK ({} checks)", r.checks.len());
    } else {
        println!("PROOF FAILED ({failed} of {})", r.checks.len());
        std::process::exit(1);
    }
}

fn png_dims(p: &std::path::Path) -> Option<(u32, u32)> {
    png::dimensions(&std::fs::read(p).ok()?)
}
