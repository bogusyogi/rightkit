//! FM-165 and FM-174 runtime projections.

use crate::support::e2e_api::Rig;
use crate::support::scripted_provider::Step;
use serde_json::json;

#[test]
fn azure_forge_is_reported_as_typed_unsupported() {
    let rig = Rig::start(vec![Step::text("ready")]);
    let (status, response) = rig.get("/delivery/forges");
    assert_eq!(status, 200, "forge capability list: {response}");
    let azure = response["forges"]
        .as_array()
        .unwrap()
        .iter()
        // Forge::as_str() owns delivery wire identity (github_app/forge.rs),
        // independently of camelCase response field names. FM-165 retains
        // Azure's reserved/unsupported capability contract.
        .find(|forge| forge["forge"] == "azure_devops")
        .expect("Azure DevOps capability row");
    assert_eq!(
        azure["available"], false,
        "unsupported Azure leg must not claim availability: {azure}"
    );
    assert!(
        azure["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "typed unsupported reason required: {azure}"
    );
}

#[test]
fn cross_midnight_quiet_hours_policy_round_trips_through_attention_api() {
    let rig = Rig::start(vec![Step::text("ready")]);
    let workspace_root = rig.ws().to_string_lossy().to_string();
    let (status, saved) = rig.patch(
        "/attention/quiet-hours",
        json!({
            "workspaceRoot":workspace_root,
            "enabled":true,
            "windows":[{"start":"22:30","end":"06:15"}]
        }),
    );
    assert_eq!(status, 200, "save quiet hours: {saved}");
    assert_eq!(saved["enabled"], true);
    assert_eq!(saved["windows"][0]["start"], "22:30");
    assert_eq!(saved["windows"][0]["end"], "06:15");

    let encoded_root = workspace_root
        .replace('%', "%25")
        .replace('\\', "%5C")
        .replace('/', "%2F")
        .replace(':', "%3A")
        .replace(' ', "%20");
    let (status, current) = rig.get(&format!(
        "/attention/quiet-hours?workspaceRoot={}",
        encoded_root
    ));
    assert_eq!(status, 200, "read quiet hours: {current}");
    assert_eq!(current, saved, "quiet-hours projection must be durable");
}
