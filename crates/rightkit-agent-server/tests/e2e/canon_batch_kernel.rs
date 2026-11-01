//! Process topology and kernel profile black-box checks.

use crate::support::{e2e_api::Rig, e2e_cli};
use serde_json::Value;

#[test]
fn krn009_practical_profile_keeps_one_resident_host_and_lazy_helpers() {
    let rig = Rig::start(vec![]);
    let (status, snapshot) = rig.get("/runtime/process-topology?profile=practical");
    assert_eq!(status, 200, "topology sampler: {snapshot}");
    assert_eq!(snapshot["profile"], "practical");
    let processes = snapshot["processes"]
        .as_array()
        .expect("sampled process rows");
    let coderight: Vec<_> = processes
        .iter()
        .filter(|p| p["owner"] == "coderight")
        .collect();
    assert_eq!(
        coderight.iter().filter(|p| p["role"] == "resident").count(),
        1,
        "one CodeRight resident: {snapshot}"
    );
    assert!(
        coderight
            .iter()
            .filter(|p| p["role"] == "helper")
            .all(|p| p["invoked"] == true),
        "only invoked helper trees are present: {snapshot}"
    );
}

#[test]
fn krn009_invoked_helper_is_reaped_without_replacing_resident_process() {
    let rig = Rig::start(vec![]);
    let (_, before) = rig.get("/runtime/process-topology?profile=practical");
    let start: Value = before;
    let pid = start["processes"]
        .as_array()
        .and_then(|p| p.iter().find(|p| p["role"] == "resident"))
        .and_then(|p| p["pid"].as_u64())
        .expect("resident PID");
    let (status, _) = rig.post(
        "/execution/processes",
        serde_json::json!({"sessionId":"topology-missing"}),
    );
    assert_eq!(
        status, 404,
        "invalid helper request is denied before launch"
    );
    let (_, after) = rig.get("/runtime/process-topology?profile=practical");
    assert!(
        after["processes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["role"] == "resident" && p["pid"] == pid),
        "resident remains stable"
    );
}

#[test]
fn krn009_cli_inline_execution_does_not_create_a_second_resident_service() {
    let p = e2e_cli::Provider::start(vec![e2e_cli::Reply::Chat(e2e_cli::Chat::text("inline"))]);
    let cli = e2e_cli::Cli::new(&p);
    let out = cli.run(&["--stream-json"], "hello");
    assert_eq!(out.code, Some(0), "{}", out.diag());
    assert_eq!(p.count(), 1);
    assert!(
        !crate::common::app_root(cli.home.path())
            .join("daemon.port")
            .exists(),
        "inline local execution remains in-process"
    );
}
