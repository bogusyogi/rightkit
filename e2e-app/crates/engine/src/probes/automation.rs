//! rightkit-automation: the safety policy (allowlist, deny, kill switch, risk and credential classifiers, redaction) on
//! known inputs, then the real accessibility stack against a spawned helper (macOS: TextEdit through AX; Windows:
//! Notepad through UIA): launch without stealing focus, snapshot the tree, write and read back text, deliver text with
//! read-back proof, receipts and journal events, allowlist/kill-switch/lease refusals on the live target, a redacted
//! window screenshot, and a clean quit. macOS needs Accessibility and Screen Recording granted to the engine's host
//! process; Windows UIA needs no grant but an interactive desktop. The helper is always quit, even on failure.
use std::sync::Arc;
use std::time::Duration;

use rightkit_automation::policy::redact;
use rightkit_automation::{
    classify_risk, credential_label, kill, killed, looks_secret, native, reset_kill, ActOptions,
    Action, AppInfo, Automation, Error, Guard, JsonlEvidence, LeaseRequest, Limits, MemoryEvidence,
    MemoryIndicator, MemoryJournal, NativePlatform, Policy, ResultKind, RiskClass, Snapshot,
    Target, Tier,
};
use serde_json::{json, Value};

#[cfg(target_os = "macos")]
const HELPER: &str = "com.apple.TextEdit";
#[cfg(windows)]
const HELPER: &str = "notepad.exe";
#[cfg(not(any(target_os = "macos", windows)))]
const HELPER: &str = "";

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// The kill switch is process-wide: always release it, whatever happens in between.
struct KillGuard;
impl Drop for KillGuard {
    fn drop(&mut self) {
        reset_kill();
    }
}

fn app(pid: i32, id: &str) -> AppInfo {
    AppInfo {
        pid,
        app_id: id.into(),
        name: id.into(),
        path: None,
        started: None,
    }
}

fn ensure(ok: bool, what: impl FnOnce() -> String) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(what())
    }
}

fn pure_checks() -> Result<Value, String> {
    let policy = Policy::new(["Allowed.App", "other.app"]);
    ensure(policy.check(&app(10, "allowed.app")).is_ok(), || {
        "an allowlisted app was refused (matching must ignore case)".into()
    })?;
    ensure(
        matches!(
            policy.check(&app(11, "stranger.app")),
            Err(Error::Blocked(_))
        ),
        || "an app off the allowlist was not Blocked".into(),
    )?;
    policy.deny("other.app");
    ensure(
        matches!(policy.check(&app(12, "other.app")), Err(Error::Blocked(_))),
        || "deny did not beat the allowlist".into(),
    )?;
    policy.protect_pid(10);
    ensure(
        matches!(
            policy.check(&app(10, "allowed.app")),
            Err(Error::Blocked(_))
        ),
        || "the protected host pid was drivable".into(),
    )?;
    policy.revoke("allowed.app");
    ensure(
        matches!(
            policy.check(&app(13, "allowed.app")),
            Err(Error::Blocked(_))
        ),
        || "a revoked app was drivable".into(),
    )?;
    {
        let _guard = KillGuard;
        let open = Policy::new(["a"]);
        kill();
        ensure(
            killed() && matches!(open.check(&app(1, "a")), Err(Error::Blocked(_))),
            || "the kill switch did not fail closed".into(),
        )?;
    }
    ensure(!killed(), || "reset_kill left the switch engaged".into())?;

    ensure(
        classify_risk("Delete account") == Some(RiskClass::Delete),
        || "'Delete account' is not Delete".into(),
    )?;
    ensure(
        classify_risk("Send message") == Some(RiskClass::Send),
        || "'Send message' is not Send".into(),
    )?;
    ensure(
        classify_risk("Buy now") == Some(RiskClass::Purchase),
        || "'Buy now' is not Purchase".into(),
    )?;
    ensure(classify_risk("Quit") == Some(RiskClass::Close), || {
        "'Quit' is not Close".into()
    })?;
    ensure(classify_risk("Bold").is_none(), || {
        "'Bold' was classified as risky".into()
    })?;
    ensure(
        credential_label("Card number")
            && credential_label("Password")
            && !credential_label("Display name"),
        || "credential_label is wrong".into(),
    )?;
    ensure(
        looks_secret("Zx9Qw3Er7Ty1Ui5Op2As4Df6Gh8Jk0") && !looks_secret("hello world"),
        || "looks_secret is wrong".into(),
    )?;
    ensure(
        redact("hunter2", true, true) == "<redacted 7 chars>",
        || "a secure field was not redacted".into(),
    )?;
    ensure(
        redact("hello", false, false) == "<5 chars>" && redact("hello", false, true) == "hello",
        || "receipt payload redaction is wrong".into(),
    )?;
    Ok(
        json!({ "policy": "allow, deny, revoke, protect, kill", "classifiers": "risk, credential, secret, redact" }),
    )
}

fn editor(s: &Snapshot) -> Option<String> {
    s.root
        .find(&|n| n.role == "textarea" || n.role == "textfield")
        .map(|n| n.id.clone())
}

fn snap(a: &Automation<NativePlatform>, pid: i32) -> Result<Snapshot, String> {
    a.snapshot(
        Target::app(pid),
        Limits {
            max_depth: 14,
            max_nodes: 900,
        },
    )
    .map_err(|e| format!("snapshot: {e}"))
}

fn editor_value(a: &Automation<NativePlatform>, pid: i32, id: &str) -> Result<String, String> {
    Ok(snap(a, pid)?
        .root
        .by_id(id)
        .and_then(|n| n.value.clone())
        .unwrap_or_default())
}

fn drive(
    a: &Automation<NativePlatform>,
    pid: i32,
    dir: &std::path::Path,
    missing: &mut Vec<String>,
) -> Result<Value, String> {
    let front0 = a.frontmost().map(|f| f.pid);
    a.begin_session(pid, "rke2e")
        .map_err(|e| format!("begin_session: {e}"))?;

    // The tree: ids stable across two immediate snapshots (the helper may still be settling, so allow a few tries).
    let mut stable = None;
    for _ in 0..4 {
        let (s1, s2) = (snap(a, pid)?, snap(a, pid)?);
        if s1.fingerprint == s2.fingerprint && s1.root.count() > 3 {
            stable = Some((s2, s1.root.count()));
            break;
        }
        sleep(500);
    }
    let (mut tree, nodes) = stable.ok_or("the helper's accessibility tree never gave two identical fingerprints with more than 3 nodes")?;

    if editor(&tree).is_none() && cfg!(target_os = "macos") {
        let o = a
            .act(
                &tree.id,
                None,
                Action::MenuPress(vec!["File".into(), "New".into()]),
                &ActOptions::default(),
            )
            .map_err(|e| format!("menu File > New: {e}"))?;
        ensure(
            o.receipt.tier_used == Some(Tier::Semantic) || o.fresh.is_some(),
            || format!("File > New ran at tier {:?}", o.receipt.tier_used),
        )?;
        sleep(900);
        tree = snap(a, pid)?;
    }
    let ed = editor(&tree)
        .ok_or_else(|| format!("no textarea or textfield in the helper's tree of {nodes} nodes"))?;

    // Tier 1: a semantic value write, read back from a fresh snapshot.
    let text = "rke2e semantic write";
    let mut outcome = a.act(
        &tree.id,
        Some(&ed),
        Action::SetValue(text.into()),
        &ActOptions::default(),
    );
    if let Ok(o) = &outcome {
        if let Some(fresh) = &o.fresh {
            outcome = a.act(
                &fresh.id,
                Some(&ed),
                Action::SetValue(text.into()),
                &ActOptions::default(),
            );
        }
    }
    sleep(150);
    let tier1 = match &outcome {
        Ok(o) if o.done() => {
            editor_value(a, pid, &ed)? == text && o.receipt.tier_used == Some(Tier::Semantic)
        }
        _ => false,
    };
    if !tier1 && cfg!(target_os = "macos") {
        return Err(format!(
            "the tier-1 AXValue write was not read back as the text written ({outcome:?})"
        ));
    }

    // Delivery with read-back proof: the product path. A pin captures the app's focused editor, so focus it first.
    let focus_tree = snap(a, pid)?;
    a.act(
        &focus_tree.id,
        Some(&ed),
        Action::Focus,
        &ActOptions::default(),
    )
    .map_err(|e| format!("focus the editor before capture_pin: {e}"))?;
    // Delivery goes only into the app the user has in front (the crate never activates a target), so do what the user
    // does: bring the helper to the front, then wait until the system reports it frontmost.
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .args(["-b", HELPER])
            .status()
            .map_err(|e| format!("open -b {HELPER}: {e}"))?;
        let mut front = false;
        for _ in 0..40 {
            if a.frontmost().map(|f| f.pid) == Some(pid) {
                front = true;
                break;
            }
            sleep(100);
        }
        if !front {
            return Err(format!("{HELPER} did not come to the front for delivery"));
        }
    }
    sleep(150);
    let pin = a
        .capture_pin(Some(pid))
        .map_err(|e| format!("capture_pin: {e}"))?;
    let delivered = a.deliver_text(
        &pin,
        " + delivered",
        &rightkit_automation::DeliveryOptions::default(),
    );
    ensure(delivered.inserted(), || {
        format!(
            "deliver_text was not confirmed by read-back: {:?} via {:?}",
            delivered.outcome,
            delivered
                .attempts
                .iter()
                .map(|x| (x.tier, x.ok))
                .collect::<Vec<_>>()
        )
    })?;

    // Refusals on the live target: allowlist revoke, kill switch, lease fence.
    a.policy().revoke(HELPER);
    let revoked = a.snapshot(Target::app(pid), Limits::default());
    a.policy().allow(HELPER);
    ensure(matches!(revoked, Err(Error::Blocked(_))), || {
        format!(
            "a revoked helper was still observable: {:?}",
            revoked.map(|s| s.id)
        )
    })?;
    {
        let _guard = KillGuard;
        kill();
        let blocked = a.snapshot(Target::app(pid), Limits::default());
        ensure(matches!(blocked, Err(Error::Blocked(_))), || {
            "the kill switch did not block a live snapshot".into()
        })?;
    }
    let tree = snap(a, pid)?;
    let ed = editor(&tree).unwrap_or(ed);

    let lease = a
        .acquire_lease(pid, LeaseRequest::new("rke2e"))
        .map_err(|e| format!("acquire_lease: {e}"))?;
    let rival = a.acquire_lease(pid, LeaseRequest::new("rival"));
    let unfenced = a.act(
        &tree.id,
        Some(&ed),
        Action::Focus,
        &ActOptions {
            guard: Guard::Off,
            ..Default::default()
        },
    );
    let fenced = a.act(
        &tree.id,
        Some(&ed),
        Action::Focus,
        &ActOptions {
            guard: Guard::Off,
            fence: Some(lease.fence(1)),
            ..Default::default()
        },
    );
    let replay = a.act(
        &tree.id,
        Some(&ed),
        Action::Focus,
        &ActOptions {
            guard: Guard::Off,
            fence: Some(lease.fence(1)),
            ..Default::default()
        },
    );
    a.release_lease(&lease);
    ensure(matches!(rival, Err(Error::LeaseHeld { .. })), || {
        format!("a second holder got the lease: {rival:?}")
    })?;
    ensure(
        matches!(&unfenced, Err(_)) || matches!(&unfenced, Ok(o) if !o.done()),
        || "an input without a fence ran while the app was leased".into(),
    )?;
    ensure(
        matches!(&fenced, Ok(o) if o.done() || o.fresh.is_some()),
        || format!("the fenced input was refused: {fenced:?}"),
    )?;
    ensure(
        matches!(&replay, Err(Error::Replayed { .. })) || matches!(&replay, Ok(o) if !o.done()),
        || format!("a replayed fence seq was executed again: {replay:?}"),
    )?;

    // A window screenshot (redacted PNG).
    let shot = dir.join("helper.png");
    let status = a.status();
    if status.screen_capture {
        a.screenshot(&Target::app(pid), &shot)
            .map_err(|e| format!("screenshot: {e}"))?;
        let bytes = std::fs::read(&shot).map_err(|e| format!("read screenshot: {e}"))?;
        ensure(
            bytes.starts_with(&[0x89, b'P', b'N', b'G']) && bytes.len() > 1000,
            || {
                format!(
                    "the screenshot is {} bytes without a PNG header",
                    bytes.len()
                )
            },
        )?;
    } else {
        missing.push(
            "needs Screen Recording granted to the engine's host process (window screenshots)"
                .into(),
        );
    }

    // Receipts, journal and counters saw all of it.
    let m = a.metrics();
    ensure(
        m.sessions_started == 1 && m.acts >= 3 && m.denied >= 2 && m.deliveries == 1,
        || format!("metrics do not match what ran: {m:?}"),
    )?;

    let front1 = a.frontmost().map(|f| f.pid);
    if let (Some(before), Some(after)) = (front0, front1) {
        ensure(before == after, || {
            format!("driving the helper moved the foreground from pid {before} to {after}")
        })?;
    }
    Ok(json!({
        "nodes": nodes, "tier1_set_value": tier1, "deliver_text": format!("{:?}", delivered.outcome),
        "acts": m.acts, "acts_done": m.acts_done, "denied": m.denied, "screenshot": status.screen_capture,
        "frontmost_checked": front0.is_some() && front1.is_some(),
    }))
}

fn live_checks(ctx: &super::Ctx) -> Result<Value, String> {
    if HELPER.is_empty() {
        return Err("needs macOS (AX) or Windows (UIA)".into());
    }
    let mut dir = ctx.dir("automation")?;
    if !dir.is_absolute() {
        dir = std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(dir);
    }
    let a = native(Policy::new([HELPER]));
    a.policy().protect_pid(std::process::id() as i32);
    let evidence = Arc::new(MemoryEvidence::default());
    a.add_evidence(evidence.clone());
    a.add_evidence(Arc::new(
        JsonlEvidence::open(&dir.join("evidence.jsonl"))
            .map_err(|e| format!("open evidence file: {e}"))?,
    ));
    let indicator = Arc::new(MemoryIndicator::default());
    a.set_indicator(indicator.clone());
    let journal = Arc::new(MemoryJournal::default());
    a.add_journal(journal.clone());

    let status = a.status();
    if !status.accessibility {
        return Err("needs Accessibility granted to the engine's host process (System Settings > Privacy & Security > Accessibility)".into());
    }
    let mut missing: Vec<String> = Vec::new();

    let front0 = a.frontmost().map(|f| f.pid);
    let helper = a
        .launch(HELPER)
        .map_err(|e| format!("launch {HELPER}: {e}"))?;
    sleep(1500);
    let outcome = (|| {
        if let (Some(before), Some(after)) = (front0, a.frontmost().map(|f| f.pid)) {
            ensure(before == after, || {
                format!("launching {HELPER} took the foreground (pid {before} -> {after})")
            })?;
        }
        drive(&a, helper.pid, &dir, &mut missing)
    })();

    // Always quit the helper; a "save changes" sheet is discarded with an explicit confirmation.
    a.end_session(helper.pid);
    let confirm = ActOptions {
        confirm: vec![RiskClass::Close, RiskClass::Delete],
        guard: Guard::Off,
        ..Default::default()
    };
    let mut closed = false;
    for _ in 0..4 {
        let _ = a.terminate(helper.pid);
        sleep(800);
        if !a.apps().iter().any(|x| x.pid == helper.pid) {
            closed = true;
            break;
        }
        if let Ok(s) = a.snapshot(
            Target::app(helper.pid),
            Limits {
                max_depth: 14,
                max_nodes: 900,
            },
        ) {
            let button = s.root.find(&|n| {
                n.role == "button"
                    && matches!(
                        n.name.as_deref(),
                        Some("Delete")
                            | Some("Don\u{2019}t Save")
                            | Some("Don't Save")
                            | Some("Don\u{2019}t save")
                            | Some("Don't save")
                    )
            });
            if let Some(b) = button {
                let _ = a.act(&s.id, Some(&b.id.clone()), Action::Press, &confirm);
                sleep(400);
            }
        }
    }
    let mut detail = outcome?;
    ensure(closed, || {
        format!(
            "the helper (pid {}) was still running after four quit attempts",
            helper.pid
        )
    })?;

    let receipts = evidence.all();
    ensure(
        receipts.iter().any(|r| r.result == ResultKind::Done),
        || "no receipt recorded a completed act".into(),
    )?;
    ensure(receipts.iter().any(|r| r.denied_by.is_some()), || {
        "no receipt recorded the gate that refused".into()
    })?;
    let events = journal.all().len();
    ensure(events >= 3, || {
        format!("the journal saw {events} lifecycle events")
    })?;
    ensure(!indicator.all().is_empty(), || {
        "the indicator saw no session event".into()
    })?;
    ensure(
        std::fs::metadata(dir.join("evidence.jsonl"))
            .map(|m| m.len() > 0)
            .unwrap_or(false),
        || "the JSONL evidence file is empty".into(),
    )?;
    detail["receipts"] = json!(receipts.len());
    detail["journal_events"] = json!(events);
    detail["helper_closed"] = json!(true);
    if !missing.is_empty() {
        return Err(missing.join("; "));
    }
    Ok(detail)
}

pub fn probe(ctx: &super::Ctx) -> Result<Value, String> {
    let pure = pure_checks()?;
    let live = live_checks(ctx)?;
    Ok(json!({ "pure": pure, "live": live }))
}
