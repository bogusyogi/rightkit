//! S:N3b bot authority-free export/import round-trip.

use crate::support::e2e_api::*;
use crate::support::scripted_provider::Step;
use serde_json::Value;

#[test]
fn bot_export_import_carries_capability_labels_without_runtime_authority() {
    let rig = Rig::start(vec![]);
    let mut profile: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/bots/bot-profile-v1.json"
    ))
    .expect("bot fixture");
    profile["id"] = "s-n3b-authority-free".into();
    profile["name"] = "S N3b authority free".into();
    profile["project"] = rig.ws().to_string_lossy().into_owned().into();

    let (status, created) = rig.post("/bots", profile);
    assert_eq!(status, 201, "create bot profile: {created}");

    let (status, package) = rig.get("/bots/s-n3b-authority-free/export");
    assert_eq!(status, 200, "export bot: {package}");
    assert_eq!(package["provenance"], "coderight:authority-free-export");
    assert!(package["contentHash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    assert!(package["schedules"].as_array().is_some_and(Vec::is_empty));
    for forbidden in [
        "rolePack",
        "model",
        "permissions",
        "toolGrants",
        "verifier",
        "enabled",
    ] {
        assert!(
            package.get(forbidden).is_none(),
            "authority-bearing field {forbidden} escaped export: {package}"
        );
    }

    let (status, imported) = rig.post("/bots/import", package.clone());
    assert_eq!(status, 201, "import authority-free bot package: {imported}");
    assert_eq!(
        imported["enabled"], false,
        "imports must not inherit execution authority: {imported}"
    );
    assert_eq!(imported["rolePack"], "imported-disabled");
    assert_eq!(imported["model"], "catalog/default");
    assert_eq!(imported["tools"], package["capabilityLabels"]);
}

#[test]
fn required_post_run_verifier_rejection_keeps_bot_run_failed() {
    let rig = Rig::start(vec![
        Step::text("scheduled work completed"),
        Step::text("VERDICT: REJECT: result lacks evidence"),
    ]);
    let mut profile: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/bots/bot-profile-v1.json"
    ))
    .expect("bot fixture");
    profile["id"] = "s-n2-verifier".into();
    profile["name"] = "S N2 verifier".into();
    profile["project"] = rig.ws().to_string_lossy().into_owned().into();
    // Bind fixture catalog labels to this rig's real loopback provider route.
    profile["model"] = MODEL.into();
    profile["verifier"]["model"] = MODEL.into();

    let (status, created) = rig.post("/bots", profile);
    assert_eq!(status, 201, "create verifier bot: {created}");
    let (status, submitted) = rig.post("/bots/s-n2-verifier/run", serde_json::json!({}));
    assert_eq!(status, 200, "run verifier bot: {submitted}");
    let schedule_id = submitted["scheduleId"]
        .as_str()
        .expect("bot run schedule id");
    let runs = rig.wait("terminal verifier result", 60, || {
        let (status, body) = rig.get(&format!("/schedules/{schedule_id}/runs"));
        (status == 200
            && matches!(
                body["latestRun"]["status"].as_str(),
                Some("completed" | "failed" | "cancelled")
            ))
        .then_some(body)
    });
    assert_eq!(
        runs["latestRun"]["status"], "failed",
        "required rejection must fail closed: {runs}"
    );
    let message = runs["latestRun"]["message"]
        .as_str()
        .expect("structured terminal message");
    let terminal: Value = serde_json::from_str(message).expect("terminal JSON");
    assert_eq!(
        terminal["verifierVerdict"]["kind"], "rejected",
        "{terminal}"
    );
}

#[test]
fn each_bot_run_has_a_fresh_task_isolation_namespace() {
    let rig = Rig::start(vec![
        Step::text("first occurrence"),
        Step::text("second occurrence"),
    ]);
    let mut profile: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/bots/bot-profile-v1.json"
    ))
    .expect("bot fixture");
    profile["id"] = "s-n3a-isolation".into();
    profile["name"] = "S N3a isolation".into();
    profile["project"] = rig.ws().to_string_lossy().into_owned().into();
    profile["model"] = MODEL.into();
    // This journey isolates two task occurrences; verifier execution is
    // exercised independently by S:N2 above.
    profile["verifier"] = serde_json::json!({"required":false});

    let (status, created) = rig.post("/bots", profile);
    assert_eq!(status, 201, "create isolated bot: {created}");
    let run_once = || {
        let (status, submitted) = rig.post("/bots/s-n3a-isolation/run", serde_json::json!({}));
        assert_eq!(status, 200, "submit bot run: {submitted}");
        let schedule_id = submitted["scheduleId"]
            .as_str()
            .expect("bot run schedule id");
        rig.wait("terminal bot run", 60, || {
            let (status, body) = rig.get(&format!("/schedules/{schedule_id}/runs"));
            (status == 200
                && matches!(
                    body["latestRun"]["status"].as_str(),
                    Some("completed" | "failed" | "cancelled")
                ))
            .then_some(body)
        })
    };
    let first = run_once();
    let second = run_once();
    assert_eq!(
        first["latestRun"]["status"], "completed",
        "first occurrence: {first}"
    );
    assert_eq!(
        second["latestRun"]["status"], "completed",
        "second occurrence: {second}"
    );
    let isolation = |run: &Value| {
        let message = run["latestRun"]["message"]
            .as_str()
            .expect("structured terminal message");
        let terminal: Value = serde_json::from_str(message).expect("terminal JSON");
        terminal["taskIsolation"].clone()
    };
    let first_isolation = isolation(&first);
    let second_isolation = isolation(&second);
    for field in [
        "taskId",
        "transcriptId",
        "providerId",
        "browserId",
        "toolStateId",
        "budgetId",
        "evidenceId",
    ] {
        let a = first_isolation[field].as_str().expect("first namespace");
        let b = second_isolation[field].as_str().expect("second namespace");
        assert_ne!(a, b, "{field} must be fresh between bot occurrences");
    }
}
