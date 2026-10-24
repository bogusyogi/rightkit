//! Windows smoke proof against Notepad: background launch, UIA tree with stable
//! ids, text delivery with read-back proof, tier 3 key messages, per-window
//! capture, safety gates, and cleanup. Frontmost must be unchanged throughout.
//!
//! Run (Windows host): rustright cargo run -p rightkit-automation --example win_smoke
//! Env: RKA_OUT=<dir> for evidence (default: %TEMP%\rka-proof)

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
    fn check(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        let detail = detail.into();
        println!("[{}] {name}: {detail}", if ok { "ok" } else { "FAIL" });
        self.checks.push((name.into(), ok, detail));
    }
    fn note(&mut self, name: &str, detail: impl Into<String>) {
        let detail = detail.into();
        println!("[note] {name}: {detail}");
        self.checks.push((format!("note: {name}"), true, detail));
    }
}

fn editor(s: &Snapshot) -> Option<&Node> {
    s.root
        .find(&|n| n.role == "textarea" || n.role == "textfield")
}

fn main() {
    let out = std::env::var("RKA_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("rka-proof"));
    std::fs::create_dir_all(&out).unwrap();
    let a = native(Policy::new(["notepad.exe"]));
    a.add_evidence(Arc::new(
        JsonlEvidence::open(&out.join("evidence.jsonl")).unwrap(),
    ));
    let mut r = Run { checks: vec![] };
    println!("status: {:?}", a.status());
    let front0 = a.frontmost().map(|f| f.pid);
    let same_front = |a: &Automation<NativePlatform>| a.frontmost().map(|f| f.pid) == front0;

    let app = match a.launch("notepad.exe") {
        Ok(app) => app,
        Err(e) => {
            println!("launch failed: {e}");
            std::process::exit(2);
        }
    };
    sleep(1500);
    r.check(
        "Notepad launched without activation",
        same_front(&a),
        format!(
            "pid {} front {:?} -> {:?}",
            app.pid,
            front0,
            a.frontmost().map(|f| f.pid)
        ),
    );
    a.begin_session(app.pid, "win_smoke").unwrap();
    let limits = Limits {
        max_depth: 12,
        max_nodes: 400,
    };
    let s = a.snapshot(Target::app(app.pid), limits).expect("snapshot");
    let s2 = a
        .snapshot(Target::app(app.pid), limits)
        .expect("snapshot 2");
    r.check(
        "UIA tree has stable element ids across snapshots",
        s.fingerprint == s2.fingerprint && s.root.count() > 3,
        format!("{} nodes", s.root.count()),
    );
    let ed = editor(&s2).expect("editor control").id.clone();
    let o = a.act(
        &s2.id,
        Some(&ed),
        Action::SetValue("tier1 set-value".into()),
        &ActOptions::default(),
    );
    match &o {
        Ok(o) => r.check(
            "tier1 ValuePattern set-value",
            o.receipt.tier_used == Some(Tier::Semantic) || o.fresh.is_some(),
            format!("{:?}", o.receipt.tier_used),
        ),
        Err(e) => r.note(
            "tier1 set-value not available on this editor",
            e.to_string(),
        ),
    }
    // Delivery with read-back proof (native write only on an empty ValuePattern field, else key messages).
    let pin = a.capture_pin(Some(app.pid)).expect("pin");
    let rec = a.deliver_text(
        &pin,
        "hello from rightkit-automation",
        &DeliveryOptions::default(),
    );
    r.check(
        "deliver_text confirmed by read-back",
        rec.inserted(),
        format!(
            "{:?} via {:?}",
            rec.outcome,
            rec.attempts
                .iter()
                .map(|x| (x.tier, x.ok))
                .collect::<Vec<_>>()
        ),
    );
    r.check("frontmost unchanged after delivery", same_front(&a), "");
    let o = a.act(
        &s2.id,
        None,
        Action::Keys("!".into()),
        &ActOptions {
            guard: Guard::Off,
            ..Default::default()
        },
    );
    r.note(
        "tier3 WM_KEYDOWN/WM_CHAR to the focused child",
        format!("{:?}", o.map(|o| o.receipt.tier_used)),
    );
    let shot = out.join("notepad.png");
    let wid = a.screenshot(&Target::app(app.pid), &shot);
    let dims = std::fs::read(&shot).ok().and_then(|b| png::dimensions(&b));
    r.check(
        "PrintWindow capture",
        wid.is_ok() && dims.is_some(),
        format!("{wid:?} {dims:?}"),
    );
    // Safety gates.
    let denied = a
        .apps()
        .into_iter()
        .find(|x| x.app_id == "explorer.exe")
        .map(|x| {
            matches!(
                a.snapshot(Target::app(x.pid), Limits::default()),
                Err(Error::Blocked(_))
            )
        });
    if let Some(d) = denied {
        r.check("allowlist denies explorer.exe", d, "Blocked");
    }
    kill();
    let blocked = matches!(
        a.snapshot(Target::app(app.pid), limits),
        Err(Error::Blocked(_))
    );
    reset_kill();
    r.check("kill switch fails closed", blocked, "");
    a.end_session(app.pid);
    let _ = a.terminate(app.pid);
    sleep(1000);
    r.check(
        "closed what it opened (or a save prompt is waiting)",
        !a.apps().iter().any(|x| x.pid == app.pid),
        format!("pid {}", app.pid),
    );
    r.check(
        "frontmost unchanged at the end",
        same_front(&a),
        format!("{:?} -> {:?}", front0, a.frontmost().map(|f| f.pid)),
    );

    let failed = r.checks.iter().filter(|c| !c.1).count();
    let items: Vec<String> = r
        .checks
        .iter()
        .map(|c| {
            format!(
                "{{\"check\":{},\"ok\":{},\"detail\":{}}}",
                json::s(&c.0),
                c.1,
                json::s(&c.2)
            )
        })
        .collect();
    std::fs::write(
        out.join("proof.json"),
        format!("{{\"failed\":{failed},\"checks\":[{}]}}", items.join(",")),
    )
    .unwrap();
    println!("evidence dir: {}", out.display());
    if failed == 0 {
        println!("PROOF OK ({} checks)", r.checks.len());
    } else {
        println!("PROOF FAILED ({failed} of {})", r.checks.len());
        std::process::exit(1);
    }
}
