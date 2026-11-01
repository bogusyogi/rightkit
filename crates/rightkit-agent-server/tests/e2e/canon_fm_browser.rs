//! FM-017 and FM-163 black-box browser journeys.

// Reuse established real-Chrome + loopback fixture journey for browser tools.
#[path = "e2e_tools_browser.rs"]
mod browser_tools;

use crate::support::e2e_api::Rig;
use serde_json::json;

#[test]
fn browser_workspace_api_projects_session_authority() {
    let rig = Rig::start(vec![crate::support::scripted_provider::Step::text("ready")]);
    // Browser workspace routes require an allowed Browser capability even
    // for projection (api_browser_workspace::policy::browser_capability_verdict).
    rig.session("fm163", json!({"permissionMode":"bypass"}));

    let (status, workspace) = rig.get("/sessions/fm163/browser/workspace");
    assert_eq!(
        status, 200,
        "browser workspace projection must be live: {workspace}"
    );
    assert!(
        workspace["sessions"].is_array(),
        "session list must be typed: {workspace}"
    );
    assert!(
        workspace.get("profileKind").is_some(),
        "profile authority missing: {workspace}"
    );
    assert!(
        workspace.get("paused").is_some(),
        "pause projection missing: {workspace}"
    );
}
