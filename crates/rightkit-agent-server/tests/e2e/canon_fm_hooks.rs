//! Operator-facing hook contract journeys for FM-097.

use crate::support::e2e_api::Rig;
use crate::support::scripted_provider::Step;
use serde_json::json;

#[test]
fn hooks_operator_can_list_dry_run_revoke_and_restore_a_hook() {
    let rig = Rig::start(vec![Step::text("done")]);
    let (status, listed) = rig.get("/hooks/list");
    assert_eq!(status, 200, "hook list: {listed}");
    assert!(
        listed["hooks"].is_array(),
        "hook list must include rows: {listed}"
    );

    let (status, dry_run) = rig.post("/hooks/test", json!({"event":"PreToolUse"}));
    assert_eq!(status, 200, "dry-run hook contract: {dry_run}");
    assert!(
        dry_run.get("outcome").is_some(),
        "dry-run verdict missing: {dry_run}"
    );

    let hook_id = "RIGHTKIT_HOOK_PRE_TOOL_USE";
    let (status, revoked) = rig.post("/hooks/revoke", json!({"id":hook_id}));
    assert_eq!(status, 200, "revoke: {revoked}");
    assert_eq!(
        revoked["revoked"], true,
        "revoke should be reflected immediately: {revoked}"
    );
    let (status, listed) = rig.get("/hooks/list");
    assert_eq!(status, 200, "hook list after revoke: {listed}");
    assert!(
        listed["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|hook| { hook["id"] == hook_id && hook["revoked"] == true }),
        "operator projection must show revocation: {listed}"
    );
    let (status, restored) = rig.post("/hooks/revoke", json!({"id":hook_id,"restore":true}));
    assert_eq!(status, 200, "restore: {restored}");
    assert_eq!(
        restored["revoked"], false,
        "restore must clear revocation: {restored}"
    );
}
