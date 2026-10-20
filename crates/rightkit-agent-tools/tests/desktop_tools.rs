//! `desktop.*` through the toolbox gate, over the simulated desktop (`rightkit_automation::mock`):
//! the tools observe, snapshot and act on an allowlisted app, and every refusal (foreground tier,
//! an unlisted app, a risky press without `confirm`, a denied admission) has no effect on the
//! desktop. The real OS backends are covered by `rightkit-automation`'s own end-to-end tests; this
//! file proves the typed surface and its gate on every host.
#![cfg(feature = "automation")]

use rightkit_agent_tools::backends::desktop::desktop_tools;
use rightkit_agent_tools::*;
use rightkit_automation::mock::{MockPlatform, EDITOR_PID};
use rightkit_automation::{Automation, Policy};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const EDITOR: &str = "com.test.editor";

struct Rig {
    tb: Toolbox,
    mock: MockPlatform,
    journal: Arc<MemoryJournal>,
    auto: Arc<Automation<MockPlatform>>,
}

fn rig(allow: &[&str]) -> Rig {
    let mock = MockPlatform::fixture();
    let auto = Arc::new(Automation::new(
        mock.clone(),
        Policy::new(allow.iter().map(|a| a.to_string())),
    ));
    let journal = MemoryJournal::new();
    let mut tb = Toolbox::new();
    tb.register_all(desktop_tools(auto.clone())).unwrap();
    tb.allow("tester", ["desktop.*"]);
    tb.add_journal(journal.clone());
    Rig {
        tb,
        mock,
        journal,
        auto,
    }
}

fn caller() -> Caller {
    Caller::new("tester")
}

fn find_node<'a>(node: &'a Value, name: &str) -> Option<&'a Value> {
    if node["name"] == name {
        return Some(node);
    }
    node["children"]
        .as_array()?
        .iter()
        .find_map(|c| find_node(c, name))
}

/// Snapshot the editor and return `(snapshot id, element id of the button named `name`)`.
fn snapshot_and_find(rig: &Rig, name: &str) -> (String, String) {
    let snap = rig
        .tb
        .call(&caller(), "desktop.snapshot", json!({"pid": EDITOR_PID}))
        .unwrap();
    let id = find_node(&snap["tree"], name).unwrap_or_else(|| panic!("no node {name}"))["id"]
        .as_str()
        .unwrap()
        .to_string();
    (snap["snapshot"].as_str().unwrap().to_string(), id)
}

fn press(snapshot: &str, element: &str) -> Value {
    json!({"snapshot": snapshot, "element": element, "action": {"kind": "press"}})
}

#[test]
fn desktop_tools_observe_snapshot_and_act_on_an_allowlisted_app() {
    let rig = rig(&[EDITOR]);

    let status = rig.tb.call(&caller(), "desktop.status", json!({})).unwrap();
    assert_eq!(status["platform"], "Mock");
    assert_eq!(status["accessibility"], true);
    assert_eq!(status["allowed_apps"], json!([EDITOR]));

    let apps = rig.tb.call(&caller(), "desktop.apps", json!({})).unwrap();
    assert!(apps
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["pid"] == EDITOR_PID && a["app_id"] == EDITOR));

    let windows = rig
        .tb
        .call(&caller(), "desktop.windows", json!({}))
        .unwrap();
    assert_eq!(windows[0]["owner"], EDITOR);

    // The password field is in the tree, but its value never is.
    let (snapshot, save) = snapshot_and_find(&rig, "Save");
    let snap = rig
        .tb
        .call(&caller(), "desktop.snapshot", json!({"pid": EDITOR_PID}))
        .unwrap();
    let password = find_node(&snap["tree"], "Password").unwrap();
    assert_eq!(password["secure"], true);
    assert!(password["value"].is_null());

    // A semantic press lands on the semantic tier, with no foreground input and no focus change.
    let out = rig
        .tb
        .call(
            &caller(),
            "desktop.act",
            json!({"snapshot": snapshot, "element": save, "action": {"kind": "press"}, "max_tier": "semantic"}),
        )
        .unwrap();
    assert_eq!(out["done"], true);
    assert_eq!(out["receipt"]["tier_used"], "semantic");
    let log = rig.mock.log();
    assert!(
        log.iter().any(|l| l.starts_with("semantic:press:")),
        "{log:?}"
    );
    assert!(log
        .iter()
        .all(|l| !l.starts_with("foreground:") && !l.starts_with("activate:")));
    assert_eq!(rig.tb.metrics().denied, 0);
}

#[test]
fn foreground_tier_is_refused_by_the_gate_and_again_by_the_tool() {
    let rig = rig(&[EDITOR]);
    let (snapshot, save) = snapshot_and_find(&rig, "Save");
    let baseline = rig.mock.log();

    // Through the toolbox: `foreground` is not in the schema enum, so the argument check refuses
    // it before the tool or the automation engine is reached.
    let mut args = press(&snapshot, &save);
    args["max_tier"] = json!("foreground");
    let err = rig
        .tb
        .call(&caller(), "desktop.act", args.clone())
        .unwrap_err();
    assert_eq!(err.code, ERR_INVALID_ARGS);
    assert!(rig.journal.events().iter().any(|e| matches!(
        e,
        ToolEvent::Denied {
            by: DeniedBy::Arguments,
            ..
        }
    )));
    assert_eq!(rig.mock.log(), baseline, "a refused call touches nothing");
    assert!(rig.mock.with(|s| s.pressed.is_empty()));

    // The tool refuses it too when called without the gate, with the documented code.
    let tools = desktop_tools(rig.auto.clone());
    let act = tools
        .iter()
        .find(|t| t.spec().name == "desktop.act")
        .expect("desktop.act is registered");
    let cancel = CancelToken::new();
    let who = caller();
    let ctx = CallCtx {
        caller: &who,
        cancel: &cancel,
    };
    let err = act.call(&ctx, args).unwrap_err();
    assert_eq!(err.code, ERR_NOT_ALLOWED);
    assert!(err.message.contains("foreground"));
    // An unknown tier is invalid, never silently widened to the default.
    let mut args = press(&snapshot, &save);
    args["max_tier"] = json!("bogus");
    assert_eq!(act.call(&ctx, args).unwrap_err().code, ERR_INVALID_ARGS);
    assert_eq!(rig.mock.log(), baseline);

    // The schema offers no foreground tier to an agent at all.
    let spec = rig
        .tb
        .list_for("tester")
        .into_iter()
        .find(|s| s.name == "desktop.act")
        .unwrap();
    let tiers = spec.input_schema["properties"]["max_tier"]["enum"]
        .as_array()
        .unwrap();
    assert!(tiers.iter().all(|t| t != "foreground"), "{tiers:?}");
}

#[test]
fn unlisted_apps_and_unconfirmed_risky_presses_are_refused_with_no_effect() {
    // No app is on the allowlist: the snapshot itself is refused.
    let closed = rig(&[]);
    let err = closed
        .tb
        .call(&caller(), "desktop.snapshot", json!({"pid": EDITOR_PID}))
        .unwrap_err();
    assert_eq!(err.code, "blocked");
    assert!(closed.mock.log().is_empty());

    // A listed app still refuses a delete-class press until the caller confirms it.
    let rig = rig(&[EDITOR]);
    let (snapshot, delete) = snapshot_and_find(&rig, "Delete");
    let err = rig
        .tb
        .call(&caller(), "desktop.act", press(&snapshot, &delete))
        .unwrap_err();
    assert_eq!(err.code, "needs_confirmation");
    assert!(rig.mock.with(|s| s.pressed.is_empty()));
    assert!(rig
        .mock
        .log()
        .iter()
        .all(|l| !l.starts_with("semantic:") && !l.starts_with("post:")));
}

/// Allows observation, denies everything else, and counts what reached it.
struct ObserveOnly {
    approvals: AtomicUsize,
    settles: AtomicUsize,
}

impl ToolAdmission for ObserveOnly {
    fn approve(&self, req: &ToolRequest) -> Verdict {
        self.approvals.fetch_add(1, Ordering::SeqCst);
        match req.effect {
            Effect::Observe => Verdict::Allow,
            _ => Verdict::Deny("not approved".into()),
        }
    }
    fn settle(&self, _req: &ToolRequest, _ok: bool, _error_code: Option<&str>) {
        self.settles.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_denied_approval_causes_no_effect_on_the_desktop() {
    let mut rig = rig(&[EDITOR]);
    let hook = Arc::new(ObserveOnly {
        approvals: AtomicUsize::new(0),
        settles: AtomicUsize::new(0),
    });
    rig.tb.set_admission(hook.clone());

    // Observation is approved and runs.
    let (snapshot, save) = snapshot_and_find(&rig, "Save");
    let baseline = rig.mock.log();
    let apps_before = rig.mock.with(|s| s.apps.len());
    let settled_before = hook.settles.load(Ordering::SeqCst);
    let approvals_before = hook.approvals.load(Ordering::SeqCst);

    let denied: [(&str, Value); 5] = [
        ("desktop.act", press(&snapshot, &save)),
        ("desktop.launch", json!({"spec": "com.test.new"})),
        ("desktop.terminate", json!({"pid": EDITOR_PID})),
        (
            "desktop.lease_acquire",
            json!({"pid": EDITOR_PID, "holder": "agent"}),
        ),
        (
            "desktop.screenshot",
            json!({"pid": EDITOR_PID, "path": std::env::temp_dir().join("never-written.png")}),
        ),
    ];
    for (tool, args) in denied {
        let err = rig.tb.call(&caller(), tool, args).unwrap_err();
        assert_eq!(err.code, ERR_DENIED, "{tool}");
    }

    // Nothing ran: the desktop log, the app list, pressed buttons and the lease are untouched,
    // and the hook's `settle` (which follows execution) was never called for a denied call.
    assert_eq!(rig.mock.log(), baseline);
    assert_eq!(rig.mock.with(|s| s.apps.len()), apps_before);
    assert!(rig.mock.with(|s| s.pressed.is_empty()));
    assert!(!std::env::temp_dir().join("never-written.png").exists());
    assert_eq!(hook.approvals.load(Ordering::SeqCst) - approvals_before, 5);
    assert_eq!(hook.settles.load(Ordering::SeqCst), settled_before);
    assert_eq!(rig.tb.metrics().denied, 5);
    let events = rig.journal.events();
    let started_after_snapshot = events
        .iter()
        .filter(|e| matches!(e, ToolEvent::Started { tool, .. } if tool != "desktop.snapshot"))
        .count();
    assert_eq!(started_after_snapshot, 0, "a denied call never starts");
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                ToolEvent::Denied {
                    by: DeniedBy::Admission,
                    ..
                }
            ))
            .count(),
        5
    );
    // The lease was never taken.
    let lease = rig
        .tb
        .call(
            &caller(),
            "desktop.lease_status",
            json!({"pid": EDITOR_PID}),
        )
        .unwrap();
    assert_eq!(lease["leased"], false);
}
