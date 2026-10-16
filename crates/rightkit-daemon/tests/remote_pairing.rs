//! Short-code pairing (`rk-pair/1`) and forgetting a device. In-process: requests go straight
//! into `Daemon::handle` with a manual clock, no sockets.

mod common;

use std::collections::BTreeMap;

use common::*;
use rightkit_daemon::remote::pairing::{pairing_code, PairingInitiator};
use rightkit_daemon::remote::{
    PairingError, PairingOptions, PairingStage, Request, Response, TaskSpec, TaskStatus,
};
use ring::signature::KeyPair;
use serde_json::{json, Value};

fn post(h: &Harness, path: &str, body: &Value) -> Response {
    h.call(Request {
        method: "POST".into(),
        path_and_query: path.into(),
        headers: BTreeMap::new(),
        body: serde_json::to_vec(body).unwrap(),
    })
}

fn get(h: &Harness, path: &str) -> Response {
    h.call(Request {
        method: "GET".into(),
        path_and_query: path.into(),
        headers: BTreeMap::new(),
        body: Vec::new(),
    })
}

fn open(h: &Harness) {
    h.daemon
        .open_pairing(PairingOptions::new(&["venture", "link"], 300))
        .unwrap();
}

fn initiator(dev: &Dev, label: Option<&str>, binding: &[u8]) -> PairingInitiator {
    PairingInitiator::new(dev.kp.public_key().as_ref(), label, binding)
}

/// Device side through the begin and reveal requests. Returns the session id and the code the
/// device displays.
fn run_device(h: &Harness, dev: &Dev, label: Option<&str>, binding: &[u8]) -> (String, String) {
    let init = initiator(dev, label, binding);
    let begun = post(h, "/v1/pair", &init.begin_body());
    assert_eq!(begun.status, 201, "{:?}", begun.body);
    let id = begun.body["pairingId"].as_str().unwrap().to_string();
    let code = init
        .code(begun.body["nonce"].as_str().unwrap())
        .expect("a well-formed host nonce");
    let revealed = post(h, &format!("/v1/pair/{id}/reveal"), &init.reveal_body());
    assert_eq!(revealed.status, 200, "{:?}", revealed.body);
    (id, code)
}

/// A six-digit string that is not `code`.
fn wrong(code: &str) -> String {
    format!("{:06}", (code.parse::<u32>().unwrap() + 1) % 1_000_000)
}

fn begin_for(n: u8) -> Value {
    PairingInitiator::new(&[n; 32], None, b"").begin_body()
}

#[test]
fn matching_code_enrols_the_key_and_the_code_never_leaves_the_host() {
    let h = Harness::new();
    let dev = Dev::generate();

    // Closed until the owner opens a window.
    assert_eq!(
        post(&h, "/v1/pair", &initiator(&dev, None, b"").begin_body()).status,
        403
    );

    open(&h);
    let (id, shown_on_device) = run_device(&h, &dev, Some("phone"), b"");
    assert_eq!(
        get(&h, &format!("/v1/pair/{id}")).body["state"],
        "awaiting_confirmation"
    );

    // The host computes the same code, and tells only the in-process owner.
    assert_eq!(
        h.daemon.pairing_code(&id).as_deref(),
        Some(shown_on_device.as_str())
    );
    let pending = h.daemon.pending_pairings();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].stage, PairingStage::AwaitingConfirmation);
    assert_eq!(pending[0].label.as_deref(), Some("phone"));

    // A wrong code is refused and nothing is enrolled; a right one (typed with a space, as
    // read off a screen) enrols exactly the committed key.
    assert!(matches!(
        h.daemon.confirm_pairing(&id, &wrong(&shown_on_device)),
        Err(PairingError::WrongCode { remaining: 2 })
    ));
    assert!(h.daemon.device_ids().is_empty());
    let spaced = format!("{} {}", &shown_on_device[..3], &shown_on_device[3..]);
    let paired = h.daemon.confirm_pairing(&id, &spaced).unwrap();
    assert_eq!(h.daemon.device_ids(), vec![paired.device_id.clone()]);
    assert_eq!(paired.scopes, ["venture", "link"]);
    assert_eq!(paired.label.as_deref(), Some("phone"));
    let status = get(&h, &format!("/v1/pair/{id}"));
    assert_eq!(status.body["state"], "confirmed");
    assert_eq!(status.body["deviceId"], paired.device_id.as_str());

    // The key works as a device, and the window is spent: one pairing per window.
    let dev = Dev {
        id: paired.device_id.clone(),
        ..dev
    };
    assert_eq!(dev.call(&h, "GET", "/v1/uploads/up_x", b"").status, 404);
    assert_eq!(post(&h, "/v1/pair", &begin_for(7)).status, 403);
    assert!(matches!(
        h.daemon.confirm_pairing(&id, &shown_on_device),
        Err(PairingError::Closed)
    ));

    // No event carries the code.
    let all = serde_json::to_string(&h.daemon.events()).unwrap();
    assert!(!all.contains(&shown_on_device), "the code leaked into an event");
    assert_eq!(h.events("PAIRING_CONFIRMED").len(), 1);
    assert_eq!(h.events("ENROLLED").len(), 1);
}

#[test]
fn a_key_that_is_already_a_device_cannot_be_paired_again() {
    let h = Harness::new();
    let dev = h.enrol();
    h.daemon.revoke_device(&dev.id).unwrap();
    open(&h);
    let (id, code) = run_device(&h, &dev, None, b"");
    assert!(matches!(
        h.daemon.confirm_pairing(&id, &code),
        Err(PairingError::AlreadyEnrolled)
    ));
    // Still revoked: pairing did not resurrect it. Forgetting is the owner's separate step.
    assert_eq!(dev.call(&h, "GET", "/v1/uploads/up_x", b"").status, 401);
}

#[test]
fn a_man_in_the_middle_with_another_key_is_caught() {
    let h = Harness::new();
    open(&h);
    let device = Dev::generate();
    let mallory = Dev::generate();

    // The real device talks to the attacker, who plays the host with a nonce of its choosing;
    // the attacker plays the device to the real host with its own key. The two codes differ,
    // so the owner, typing what the device shows, is refused.
    let real = initiator(&device, Some("phone"), b"");
    let device_sees = real.code("00112233445566778899aabbccddeeff").unwrap();
    let (id, host_has) = run_device(&h, &mallory, Some("phone"), b"");
    assert_ne!(device_sees, host_has);
    assert!(matches!(
        h.daemon.confirm_pairing(&id, &device_sees),
        Err(PairingError::WrongCode { .. })
    ));
    assert!(h.daemon.device_ids().is_empty());

    // Swapping the key after committing (the commitment names the first key) is refused at the
    // reveal and burns the session.
    let honest = initiator(&device, None, b"");
    let mut swapped = honest.begin_body();
    swapped["publicKey"] = initiator(&mallory, None, b"").begin_body()["publicKey"].clone();
    let began = post(&h, "/v1/pair", &swapped);
    assert_eq!(began.status, 201);
    let id2 = began.body["pairingId"].as_str().unwrap().to_string();
    let r = post(&h, &format!("/v1/pair/{id2}/reveal"), &honest.reveal_body());
    assert_eq!(
        (r.status, r.body["error"].as_str()),
        (403, Some("commit_mismatch"))
    );
    assert_eq!(get(&h, &format!("/v1/pair/{id2}")).body["state"], "failed");
    assert!(h.daemon.pairing_code(&id2).is_none());
}

#[test]
fn the_host_binding_is_part_of_the_code() {
    let h = Harness::new();
    h.daemon
        .open_pairing(PairingOptions::new(&["venture"], 300).with_host_binding(b"cert-A".to_vec()))
        .unwrap();
    let dev = Dev::generate();
    // The device saw another certificate than the host's binding names: the codes differ.
    let (id, on_device) = run_device(&h, &dev, None, b"cert-B");
    assert_ne!(h.daemon.pairing_code(&id).unwrap(), on_device);
    assert_eq!(
        pairing_code(&[1; 32], &[2; 16], &[3; 16], None, b"x").len(),
        6
    );
}

#[test]
fn sessions_and_windows_are_capped_and_expire() {
    let h = Harness::new();
    let mut opts = PairingOptions::new(&["venture"], 300);
    opts.max_begins = 3;
    opts.max_pending = 2;
    opts.session_secs = 60;
    h.daemon.open_pairing(opts).unwrap();
    assert!(matches!(
        h.daemon.open_pairing(PairingOptions::new(&["venture"], 5)),
        Err(PairingError::AlreadyOpen)
    ));

    // Pending sessions are capped; the refused one did not count as a begin.
    assert_eq!(post(&h, "/v1/pair", &begin_for(1)).status, 201);
    assert_eq!(post(&h, "/v1/pair", &begin_for(2)).status, 201);
    let full = post(&h, "/v1/pair", &begin_for(3));
    assert_eq!((full.status, full.body["error"].as_str()), (429, Some("too_many_pending")));

    // A session is gone after its time, which frees its slot.
    h.advance(61);
    assert!(h.daemon.pending_pairings().is_empty());
    assert_eq!(post(&h, "/v1/pair", &begin_for(4)).status, 201);

    // The window accepts a fixed number of sessions in total.
    let spent = post(&h, "/v1/pair", &begin_for(5));
    assert_eq!((spent.status, spent.body["error"].as_str()), (429, Some("too_many_attempts")));

    // A window that is not used lapses, and its sessions go with it.
    h.advance(300);
    assert_eq!(post(&h, "/v1/pair", &begin_for(6)).status, 403);
    assert!(h.daemon.pending_pairings().is_empty());
    assert_eq!(h.events("PAIRING_CLOSED").len(), 1);
}

#[test]
fn wrong_codes_burn_the_session_and_repeated_failures_close_the_window() {
    let h = Harness::new();
    let mut opts = PairingOptions::new(&["venture"], 300);
    opts.max_wrong_codes = 2;
    opts.max_failures = 2;
    h.daemon.open_pairing(opts).unwrap();
    let dev = Dev::generate();
    let (id, code) = run_device(&h, &dev, None, b"");

    assert!(matches!(
        h.daemon.confirm_pairing(&id, &wrong(&code)),
        Err(PairingError::WrongCode { remaining: 1 })
    ));
    // A malformed entry counts the same as a wrong one.
    assert!(matches!(
        h.daemon.confirm_pairing(&id, "12"),
        Err(PairingError::SessionBurned)
    ));
    assert!(h.daemon.device_ids().is_empty());
    // The budget is spent: the right code no longer works, and the window is closed.
    assert!(matches!(
        h.daemon.confirm_pairing(&id, &code),
        Err(PairingError::Closed)
    ));
    assert_eq!(post(&h, "/v1/pair", &begin_for(9)).status, 403);
    assert_eq!(get(&h, &format!("/v1/pair/{id}")).body["state"], "failed");
    assert_eq!(h.events("PAIRING_CLOSED").len(), 1);
    assert_eq!(h.events("PAIRING_CLOSED")[0]["reason"], "locked");
}

#[test]
fn malformed_and_misrouted_pairing_requests_are_refused() {
    let h = Harness::new();
    open(&h);
    assert_eq!(post(&h, "/v1/pair", &json!({ "publicKey": "x" })).status, 400);
    assert_eq!(get(&h, "/v1/pair").status, 405);
    assert_eq!(get(&h, "/v1/pair/not-an-id").status, 404);
    assert_eq!(
        post(
            &h,
            "/v1/pair/pr_00000000000000000000000000000000/reveal",
            &json!({ "nonce": "00" })
        )
        .status,
        400
    );
    assert_eq!(
        post(
            &h,
            "/v1/pair/pr_00000000000000000000000000000000/reveal",
            &json!({ "nonce": "00112233445566778899aabbccddeeff" })
        )
        .status,
        404
    );
    // A label over the bound is refused, never truncated.
    let mut body = begin_for(1);
    body["label"] = json!("x".repeat(65));
    assert_eq!(post(&h, "/v1/pair", &body).status, 400);

    // Revealing twice is not a state transition; the owner can decline.
    let dev = Dev::generate();
    let init = initiator(&dev, None, b"");
    let b = post(&h, "/v1/pair", &init.begin_body());
    let id = b.body["pairingId"].as_str().unwrap().to_string();
    assert_eq!(
        post(&h, &format!("/v1/pair/{id}/reveal"), &init.reveal_body()).status,
        200
    );
    assert_eq!(
        post(&h, &format!("/v1/pair/{id}/reveal"), &init.reveal_body()).status,
        409
    );
    assert!(h.daemon.reject_pairing(&id));
    assert!(!h.daemon.reject_pairing(&id));
    assert_eq!(get(&h, &format!("/v1/pair/{id}")).body["state"], "rejected");
    assert!(h.daemon.pairing_code(&id).is_none());
    assert!(matches!(
        h.daemon.confirm_pairing(&id, "000000"),
        Err(PairingError::UnknownSession)
    ));
}

#[test]
fn forgetting_removes_every_trace_of_a_revoked_device_and_is_repeatable() {
    let mut h = Harness::new();
    let dev = h.enrol_scopes(&["venture", "link"]);
    let other = h.enrol_scopes(&["venture", "link"]);

    // An idempotency record, an unfinished upload and a task pinned to the device.
    let job = dev.patch_job(&h, "job-1", 0, "idem-1", &json!({ "x": 1 }));
    assert_eq!(job.status, 200, "{:?}", job.body);
    let (up, _key) = dev.upload_init(&h, 8, 4);
    let pinned = h
        .daemon
        .enqueue_task(TaskSpec::new("m", json!({})).target(dev.id.clone()))
        .unwrap();
    assert_eq!(h.daemon.metrics().idempotency_records, 1);

    h.daemon.revoke_device(&dev.id).unwrap();
    assert!(h.daemon.forget_device(&dev.id).unwrap());
    assert!(!h.daemon.forget_device(&dev.id).unwrap());
    assert_eq!(h.daemon.device_ids(), vec![other.id.clone()]);
    assert_eq!(h.daemon.metrics().idempotency_records, 0);
    let t = h.daemon.task(&pinned).unwrap();
    assert_eq!(t.status, TaskStatus::Failed);
    assert_eq!(t.outcome.unwrap()["error"], "device_forgotten");
    assert_eq!(dev.call(&h, "GET", "/v1/uploads/up_x", b"").status, 401);

    // The removal is durable.
    h.restart();
    assert_eq!(h.daemon.device_ids(), vec![other.id.clone()]);

    // The same key enrols as a new device: it owns none of the old upload, and the old
    // idempotency key is not replayed (the job moved on, so the stale if-match conflicts).
    let code = h.daemon.issue_enrolment_code(&["venture"], 600).unwrap();
    let again = h.call(dev.enrol_request(&code, h.now(), &next_nonce()));
    assert_eq!(again.status, 201, "{:?}", again.body);
    assert_eq!(
        dev.call(&h, "GET", &format!("/v1/uploads/{up}"), b"").status,
        404
    );
    let replay = dev.patch_job(&h, "job-1", 0, "idem-1", &json!({ "x": 1 }));
    assert_eq!(replay.status, 409, "{:?}", replay.body);
}
