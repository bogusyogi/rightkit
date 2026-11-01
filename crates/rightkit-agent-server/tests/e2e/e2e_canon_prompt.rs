//! Canon closure journeys for what the model actually receives, observed from the
//! independent scripted provider's recorded request bodies (real `coderight run`).
//!
//! Evidence map (ledger: docs/pending/canon-closure-ledger.md):
//! - `FM-069` session-start environment snapshot: present on the first provider request
//!   and deterministic (byte-identical) on every later request of the same session.
//! - `FM-073` deferred tool loading: the first request ships only the core prelude,
//!   `tool_search` promotes schemas, and promotion is monotonic for the session.

use crate::support::e2e_cli;
use crate::support::e2e_tools as t;

use e2e_cli::{Chat, Cli, Provider};
use serde_json::{json, Value};

fn snapshot_block(req: &Value) -> Option<String> {
    let s = req.to_string();
    let start = s.find("<session_start_snapshot>")?;
    let end = s[start..].find("</session_start_snapshot>")? + start;
    Some(s[start..end].to_string())
}

#[test]
fn session_start_snapshot_is_present_and_deterministic_across_provider_turns() {
    let p = Provider::start(vec![
        Chat::tool("c1", "read_file", json!({"path": "README.md"})).into(),
        Chat::text("snapshot checked").into(),
    ]);
    let cli = Cli::new(&p);
    let out = cli.run(&["--stream-json"], "inspect the workspace");
    assert_eq!(out.code, Some(0), "{}", out.diag());

    let reqs = p.requests();
    assert!(
        reqs.len() >= 2,
        "two provider turns expected: {}",
        reqs.len()
    );
    let first = snapshot_block(&reqs[0]).expect("first request carries the session-start snapshot");
    for needle in [
        "Workspace root:",
        "Git branch:",
        "Workspace manifests:",
        "Key env:",
    ] {
        assert!(first.contains(needle), "snapshot lacks {needle:?}: {first}");
    }
    let second = snapshot_block(&reqs[1]).expect("later requests keep the snapshot in context");
    assert_eq!(
        first, second,
        "the snapshot is captured once and replayed byte-identically, never recomputed per turn"
    );
}

#[test]
fn tool_search_promotes_deferred_schemas_monotonically_for_the_session() {
    let p = t::sequence(
        vec![
            t::call("tool_search", json!({"query": "git history diff"})),
            t::call("tool_search", json!({"query": "edit notebook cells"})),
        ],
        "done",
    );
    let rig = t::Rig::new(&p, &[]);
    let out = rig.run("find tools");
    assert_eq!(out.code, Some(0), "{}", out.diag());

    let reqs = p.requests();
    assert!(
        reqs.len() >= 3,
        "three provider turns expected: {}",
        reqs.len()
    );
    let names: Vec<Vec<String>> = reqs.iter().map(t::advertised_tools).collect();

    assert!(
        names[0].iter().any(|n| n == "tool_search"),
        "the prelude must ship tool_search: {:?}",
        names[0]
    );
    assert!(
        !names[0].iter().any(|n| n == "notebook"),
        "deferred tools must not ship in the first request: {:?}",
        names[0]
    );
    assert!(
        names[1].len() > names[0].len(),
        "a successful tool_search promotes deferred schemas into the next request: {:?} -> {:?}",
        names[0],
        names[1]
    );
    for later in 1..names.len() {
        for earlier_name in &names[later - 1] {
            assert!(
                names[later].contains(earlier_name),
                "promotion is monotonic: {earlier_name} vanished at request {later}"
            );
        }
    }
}
