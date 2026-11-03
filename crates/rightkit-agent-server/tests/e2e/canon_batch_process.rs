//! Execution-channel and PTY process contracts (PTY-001).

use crate::support::e2e_api::Rig;
use serde_json::json;

struct Cleanup<'a> {
    rig: &'a Rig,
    id: String,
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = self
            .rig
            .post(&format!("/execution/processes/{}/kill", self.id), json!({}));
    }
}

#[test]
fn pty001_interactive_channel_resize_eof_cancel_and_cleanup_share_process_authority() {
    let rig = Rig::start(vec![]);
    rig.session("pty-contract", json!({}));
    let (status, created) = rig.post("/execution/processes", json!({"sessionId":"pty-contract","executionChannel":"pty","ptySize":{"rows":24,"cols":80,"pixelWidth":0,"pixelHeight":0}}));
    assert_eq!(status, 200, "PTY creation: {created}");
    let process = &created["process"];
    let id = process["id"].as_str().expect("process id").to_string();
    let _cleanup = Cleanup {
        rig: &rig,
        id: id.clone(),
    };
    assert_eq!(process["executionChannel"], "pty");
    let (status, resized) = rig.post(
        &format!("/execution/processes/{id}/resize"),
        json!({"rows":40,"cols":120,"pixelWidth":0,"pixelHeight":0}),
    );
    assert_eq!(status, 200, "resize is forwarded to managed PTY: {resized}");
    assert_eq!(resized["process"]["ptySize"]["rows"], 40);
    assert_eq!(resized["process"]["ptySize"]["cols"], 120);
    let (status, ended) = rig.post(&format!("/execution/processes/{id}/stdin"), json!({"eof":true,"leaseEpoch":process["leaseEpoch"],"inputSequence":process["nextInputSequence"]}));
    assert_eq!(status, 200, "EOF is explicit and lease-fenced: {ended}");
    let (status, cancelled) = rig.post(
        &format!("/execution/processes/{id}/kill"),
        json!({"reason":"cancel"}),
    );
    assert_eq!(status, 200, "cancel terminates process tree: {cancelled}");
}

#[test]
fn pty001_typed_resource_denial_happens_before_child_creation() {
    let rig = Rig::start(vec![]);
    rig.session("pty-deny", json!({}));
    let (status, denied) = rig.post("/execution/processes", json!({"sessionId":"pty-deny","executionChannel":"pty","ptySize":{"rows":24,"cols":80,"pixelWidth":0,"pixelHeight":0},"resourceProfile":"strict"}));
    assert!(
        status == 403 || status == 429,
        "resource governor denial is typed before spawn: {status} {denied}"
    );
    assert_eq!(
        rig.get("/execution/processes").1["processes"],
        json!([]),
        "denied PTY has no child row"
    );
}
