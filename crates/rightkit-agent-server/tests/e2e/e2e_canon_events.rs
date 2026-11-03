//! Canon closure journey for EVT-002 (durable replay truth is structurally distinct
//! from coalescible live projection deltas), driven through a REAL `coderight serve`
//! daemon: the live SSE stream carries token / tool-output deltas, the durable event
//! history never does, and that stays true after a hard restart.
//!
//! Ledger: docs/pending/canon-closure-ledger.md (EVT-002).

use crate::support::daemon_process;
use crate::support::e2e_api;
use crate::support::scripted_provider;

use e2e_api::*;
use scripted_provider::Step;
use serde_json::{json, Value};
use std::time::Duration;

const LIVE_ONLY: [&str; 2] = ["assistantDelta", "toolOutputDelta"];

#[test]
fn live_deltas_reach_subscribers_but_are_never_durable_even_across_restart() {
    use std::io::BufRead;
    let mut rig = Rig::start(vec![
        Step::tool(
            "c1",
            "shell",
            json!({"command": "echo DELTA-LIVE-ONE; sleep 1; echo DELTA-LIVE-TWO"}),
        ),
        Step::text("LIVE-ANSWER-TEXT"),
    ]);
    rig.session("evt2", json!({"permissionMode": "bypass"}));

    // Subscribe BEFORE the turn: live projections are never replayed to late joiners.
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    let (url, token) = (
        format!("{}/sessions/evt2/events", rig.url()),
        rig.token().to_string(),
    );
    std::thread::spawn(move || {
        let Some(resp) = crate::support::daemon_process::open_event_stream(&url, &token) else {
            return;
        };
        for line in std::io::BufReader::new(resp).lines() {
            let Ok(line) = line else { break };
            if let Some(data) = line.strip_prefix("data:") {
                if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        }
    });
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.send("evt2", "run it").0, 200);
    rig.wait_assistant("evt2", "LIVE-ANSWER-TEXT");

    let mut live_kinds: Vec<String> = Vec::new();
    while let Ok(v) = rx.recv_timeout(Duration::from_secs(2)) {
        live_kinds.push(kind_of(&v));
    }
    assert!(
        live_kinds.iter().any(|k| k == "toolOutputDelta"),
        "subscribers see live tool-output deltas: {live_kinds:?}"
    );
    assert!(
        live_kinds.iter().any(|k| k == "assistantDelta"),
        "subscribers see live token deltas: {live_kinds:?}"
    );

    let durable = rig.kinds("evt2");
    for live_only in LIVE_ONLY {
        assert!(
            !durable.iter().any(|k| k == live_only),
            "{live_only} is a coalescible live projection and must not be durable: {durable:?}"
        );
    }
    for fact in [
        "toolCallStarted",
        "toolResultAdded",
        "assistantMessageCompleted",
    ] {
        assert!(
            durable.iter().any(|k| k == fact),
            "durable semantic fact {fact} missing: {durable:?}"
        );
    }

    // Replay truth after a hard restart is the same semantic history, still delta-free.
    rig.restart();
    let after = rig.kinds("evt2");
    for live_only in LIVE_ONLY {
        assert!(
            !after.iter().any(|k| k == live_only),
            "{live_only} appeared in durable history after restart: {after:?}"
        );
    }
    let facts = |kinds: &[String]| -> Vec<String> {
        kinds
            .iter()
            .filter(|k| {
                [
                    "userMessageSubmitted",
                    "toolCallStarted",
                    "toolResultAdded",
                    "assistantMessageCompleted",
                ]
                .contains(&k.as_str())
            })
            .cloned()
            .collect()
    };
    assert_eq!(
        facts(&durable),
        facts(&after),
        "restart must not change the durable semantic replay truth"
    );
}
