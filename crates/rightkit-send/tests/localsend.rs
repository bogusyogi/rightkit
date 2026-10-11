//! The receiving side and the trust rules, through real TLS connections to a
//! service on this machine (loopback). Discovery is not exercised.
//!
//! The client identity and User-Agent are process-wide (as in Pulse), so every
//! test holds `serial()` for its whole body.

use rightkit_send::proto::{self, PrepareUploadResponse};
use rightkit_send::{Config, Error, Event, Identity, Policy, Service, TrustState, net};
use serde_json::{Value, json};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn temp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("rightkit-send-{name}-{}", proto::random_hex(4)))
}

fn config(save: &Path, trust: &Path, policy: Policy) -> Config {
    Config {
        product_name: "Test".into(),
        alias: "Test Mac (Test)".into(),
        port: 0,
        device_type: "desktop".into(),
        device_model: "Mac".into(),
        save_dir: save.to_path_buf(),
        trust_path: trust.to_path_buf(),
        user_agent: "TestAgent/1".into(),
        quarantine_agent: "TestAgent".into(),
        temp_suffix: ".test-part".into(),
        log_prefix: "test sharing:".into(),
        policy,
    }
}

/// A running service with its folders under a fresh temp directory.
fn start(identity: &Identity, policy: Policy) -> (Arc<Service>, PathBuf, PathBuf) {
    let (save, state) = (temp("save"), temp("state"));
    let service = Service::start(
        config(&save, &state.join("nearby_devices.json"), policy),
        identity.clone(),
        Arc::new(|_: Event| {}),
    )
    .expect("service starts");
    (Arc::new(service), save, state)
}

fn loopback() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

/// Answer the first request that arrives, after `delay`.
fn answer(service: Arc<Service>, accept: bool) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for _ in 0..400 {
            if let Some(request) = service.snapshot().incoming.first() {
                assert!(service.respond(&request.id.clone(), accept));
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("no request arrived");
    })
}

fn post(port: u16, target: &str, body: &Value) -> net::Reply {
    let bytes = body.to_string();
    net::request_json(
        loopback(),
        port,
        true,
        "",
        "POST",
        target,
        Some(bytes.as_bytes()),
        Duration::from_secs(30),
    )
    .expect("the service answers")
}

fn prepare_target() -> String {
    format!("{}/prepare-upload", proto::API)
}

fn offer(fingerprint: &str, files: Value) -> Value {
    json!({
        "info": {"alias": "Phone", "version": "2.0", "deviceType": "mobile",
                 "fingerprint": fingerprint, "port": 53317, "protocol": "https"},
        "files": files
    })
}

fn one_file(name: &str, size: u64) -> Value {
    json!({"f1": {"id": "f1", "fileName": name, "size": size, "fileType": "text/plain"}})
}

fn prepare(port: u16, file_name: &str, size: u64) -> net::Reply {
    post(port, &prepare_target(), &offer("phone-1", one_file(file_name, size)))
}

fn upload(port: u16, session: &PrepareUploadResponse, token: &str, bytes: &[u8]) -> u16 {
    let mut wire = net::connect(loopback(), port, true, "", Duration::from_secs(30)).unwrap();
    let target = format!(
        "{}/upload?sessionId={}&fileId=f1&token={}",
        proto::API,
        session.session_id,
        token
    );
    let reply = net::call(
        &mut wire,
        "POST",
        "127.0.0.1",
        &target,
        Some("application/octet-stream"),
        bytes.len() as u64,
        &mut |w| w.write_all(bytes),
    )
    .unwrap();
    wire.finish();
    reply.status
}

fn wait_for_incoming(service: &Service) -> Option<rightkit_send::Incoming> {
    for _ in 0..200 {
        if let Some(incoming) = service.snapshot().incoming.first() {
            return Some(incoming.clone());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    None
}

#[test]
fn accepted_files_are_saved_inside_the_save_folder_and_never_overwrite() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, save, state) = start(&identity, Policy::default());
    let port = service.bound_port();

    for expected in ["hello.txt", "hello (1).txt"] {
        let helper = answer(service.clone(), true);
        // A hostile name: the traversal must be dropped, not followed.
        let reply = prepare(port, "../../evil/hello.txt", 5);
        helper.join().unwrap();
        assert_eq!(reply.status, 200);
        let session: PrepareUploadResponse = serde_json::from_slice(&reply.body).unwrap();
        let token = session.files["f1"].clone();
        assert_eq!(upload(port, &session, "wrong-token", b"hello"), 403);
        assert_eq!(upload(port, &session, &token, b"hello"), 200);
        // The test sender presents no certificate, so its files are kept apart.
        let saved = save
            .join("Received (unverified)")
            .join("evil")
            .join(expected);
        assert_eq!(std::fs::read(&saved).unwrap(), b"hello");
    }
    assert!(!save.parent().unwrap().join("evil").exists());
    assert!(service.snapshot().transfers.iter().all(|t| t.state == "done"));
    let _ = std::fs::remove_dir_all(&save);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn declined_requests_get_403_and_save_nothing() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, save, state) = start(&identity, Policy::default());
    let helper = answer(service.clone(), false);
    let reply = prepare(service.bound_port(), "photo.jpg", 3);
    helper.join().unwrap();
    assert_eq!(reply.status, 403);
    assert!(!save.join("photo.jpg").exists());
    let _ = std::fs::remove_dir_all(&state);
}

#[cfg(unix)]
#[test]
fn received_files_are_0644_and_never_executable() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, save, state) = start(&identity, Policy::default());
    let port = service.bound_port();
    let helper = answer(service.clone(), true);
    let reply = prepare(port, "tool.sh", 2);
    helper.join().unwrap();
    let session: PrepareUploadResponse = serde_json::from_slice(&reply.body).unwrap();
    let token = session.files["f1"].clone();
    assert_eq!(upload(port, &session, &token, b"#!"), 200);
    let saved = save.join("Received (unverified)").join("tool.sh");
    let mode = std::fs::metadata(&saved).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o644);
    assert_eq!(mode & 0o111, 0);
    #[cfg(target_os = "macos")]
    {
        // The quarantine mark carries the configured agent name.
        let mark = std::process::Command::new("/usr/bin/xattr")
            .args(["-p", "com.apple.quarantine"])
            .arg(&saved)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&mark.stdout).contains(";TestAgent;"));
    }
    let _ = std::fs::remove_dir_all(&save);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn stored_identity_keeps_its_fingerprint_across_restarts() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let stored = (identity.cert_der.clone(), identity.key_der.clone());
    let (first, _, state) = start(&identity, Policy::default());
    assert_eq!(first.fingerprint(), identity.fingerprint);
    first.stop();
    drop(first);
    // A restart reads the stored pair back, so the fingerprint is the same.
    let restored = Identity::load(stored.0, stored.1).unwrap();
    let (second, _, _) = start(&restored, Policy::default());
    assert_eq!(second.fingerprint(), identity.fingerprint);
    assert_eq!(second.snapshot().fingerprint, identity.fingerprint);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn a_request_pending_when_its_sender_is_denied_is_declined() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, save, state) = start(&identity, Policy::default());
    let fingerprint = "a".repeat(64);
    let port = service.bound_port();
    let denier = service.clone();
    let deny = std::thread::spawn(move || {
        let incoming = wait_for_incoming(&denier).expect("a request arrives");
        assert_eq!(incoming.fingerprint, "a".repeat(64));
        denier.set_trust(&incoming.fingerprint, TrustState::Deny).unwrap();
    });
    let reply = post(port, &prepare_target(), &offer(&fingerprint, one_file("x.txt", 1)));
    deny.join().unwrap();
    assert_eq!(reply.status, 403);
    assert!(!save.join("x.txt").exists());
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn message_text_is_not_shown_before_accept() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, save, state) = start(&identity, Policy::default());
    let body = offer(
        "c".repeat(64).as_str(),
        json!({"f1": {"id": "f1", "fileName": "m.txt", "size": 11,
                      "fileType": "text/plain", "preview": "secret text"}}),
    );
    let port = service.bound_port();
    let asker = std::thread::spawn(move || post(port, &prepare_target(), &body));
    let incoming = wait_for_incoming(&service).expect("the message waits for an answer");
    assert!(incoming.is_message);
    assert!(incoming.preview.is_none());
    let card = serde_json::to_string(&incoming).unwrap();
    assert!(!card.contains("secret text"));
    assert!(service.respond(&incoming.id, true));
    assert_eq!(asker.join().unwrap().status, 204);
    let transfer = service.snapshot().transfers.into_iter().next().unwrap();
    assert_eq!(transfer.message.as_deref(), Some("secret text"));
    let _ = std::fs::remove_dir_all(&save);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn sizes_are_overflow_checked_and_capped() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, _save, state) = start(&identity, Policy::default());
    let port = service.bound_port();
    let overflow = offer(
        "d".repeat(64).as_str(),
        json!({"f1": {"id": "f1", "fileName": "a", "size": u64::MAX, "fileType": "text/plain"},
               "f2": {"id": "f2", "fileName": "b", "size": 10, "fileType": "text/plain"}}),
    );
    assert_eq!(post(port, &prepare_target(), &overflow).status, 400);

    let capped = Policy {
        max_file_bytes: 1000,
        ..Policy::default()
    };
    let (small, _save2, state2) = start(&identity, capped);
    let reply = post(
        small.bound_port(),
        &prepare_target(),
        &offer("e".repeat(64).as_str(), one_file("big.bin", 2000)),
    );
    assert_eq!(reply.status, 400);
    let _ = std::fs::remove_dir_all(&state);
    let _ = std::fs::remove_dir_all(&state2);
}

#[test]
fn an_unproven_sender_is_held_to_the_policy_limits() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let policy = Policy {
        unproven_max_files: 2,
        ..Policy::default()
    };
    let (service, _save, state) = start(&identity, policy);
    let files = json!({
        "f1": {"id": "f1", "fileName": "1", "size": 1, "fileType": "text/plain"},
        "f2": {"id": "f2", "fileName": "2", "size": 1, "fileType": "text/plain"},
        "f3": {"id": "f3", "fileName": "3", "size": 1, "fileType": "text/plain"}
    });
    let reply = post(
        service.bound_port(),
        &prepare_target(),
        &offer("f".repeat(64).as_str(), files),
    );
    assert_eq!(reply.status, 413);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn an_allowed_device_without_certificate_proof_is_not_auto_accepted() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, _save, state) = start(&identity, Policy::default());
    let fingerprint = "b".repeat(64);
    service.set_trust(&fingerprint, TrustState::Allow).unwrap();
    let port = service.bound_port();
    let asker = std::thread::spawn(move || {
        post(
            port,
            &prepare_target(),
            &offer(&"b".repeat(64), one_file("n.txt", 1)),
        )
    });
    // No certificate and no answering callback: the request still waits for the owner.
    let incoming = wait_for_incoming(&service).expect("the request is asked about");
    assert_eq!(incoming.fingerprint, fingerprint);
    assert!(!incoming.verified);
    assert!(service.respond(&incoming.id, false));
    assert_eq!(asker.join().unwrap().status, 403);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn a_certificate_proven_allowed_device_is_auto_accepted_with_clipboard() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, save, state) = start(&identity, Policy::default());
    let phone = Identity::generate("Phone").unwrap();
    // The client presents its certificate, so the sender proves its key.
    net::set_client_identity(&phone);
    service.set_trust(&phone.fingerprint, TrustState::Allow).unwrap();
    let mut body = offer(&phone.fingerprint, one_file("pasted.txt", 5));
    body["pulse"] = json!({"clipboard": true});
    let reply = post(service.bound_port(), &prepare_target(), &body);
    assert_eq!(reply.status, 200);
    assert!(service.snapshot().incoming.is_empty(), "no card for an auto-accept");
    let session: PrepareUploadResponse = serde_json::from_slice(&reply.body).unwrap();
    let token = session.files["f1"].clone();
    assert_eq!(upload(service.bound_port(), &session, &token, b"hello"), 200);
    // Proven files are kept with the owner's own, not in the unverified folder.
    assert_eq!(std::fs::read(save.join("pasted.txt")).unwrap(), b"hello");
    let transfer = service.snapshot().transfers.into_iter().next().unwrap();
    assert!(transfer.clipboard);
    let _ = std::fs::remove_dir_all(&save);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn a_peer_fed_by_a_host_needs_a_64_hex_fingerprint() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, _save, state) = start(&identity, Policy::default());
    let host: IpAddr = "10.255.255.1".parse().unwrap();
    let txt = vec![("px".to_string(), "1".to_string())];
    assert_eq!(
        service.peer_add_resolved("abc", "Phone", host, 53317, &txt),
        Err(Error::InvalidFingerprint)
    );
    service
        .peer_add_resolved(&"A".repeat(64), "Phone", host, 53317, &txt)
        .unwrap();
    let device = service
        .devices()
        .into_iter()
        .find(|d| d.fingerprint == "a".repeat(64))
        .expect("the peer is listed");
    assert!(device.px);
    assert!(service.peer_remove(&"a".repeat(64)));
    assert!(!service.peer_remove(&"a".repeat(64)));
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn a_port_held_by_another_listener_is_a_typed_error() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, _save, state) = start(&identity, Policy::default());
    let (save2, state2) = (temp("save2"), temp("state2"));
    let port = service.bound_port();
    let mut taken = config(&save2, &state2.join("nearby_devices.json"), Policy::default());
    taken.port = port;
    let second = Service::start(taken, identity.clone(), Arc::new(|_: Event| {}));
    assert!(matches!(second, Err(Error::PortInUse { port: p }) if p == port));
    let _ = std::fs::remove_dir_all(&state);
    let _ = std::fs::remove_dir_all(&state2);
}

// ---- partial-file ownership (audit F01) -------------------------------------

/// Run one accepted single-file transfer to the point of having an upload
/// session, returning it and its token.
fn accepted_session(service: &Arc<Service>, port: u16, name: &str, size: u64)
    -> (PrepareUploadResponse, String)
{
    accepted_session_from(service, port, "phone-1", name, size)
}

/// Like `accepted_session`, from the sender with fingerprint `sender` (one session per sender).
fn accepted_session_from(
    service: &Arc<Service>,
    port: u16,
    sender: &str,
    name: &str,
    size: u64,
) -> (PrepareUploadResponse, String) {
    let helper = answer(service.clone(), true);
    let reply = post(port, &prepare_target(), &offer(sender, one_file(name, size)));
    helper.join().unwrap();
    assert_eq!(reply.status, 200);
    let session: PrepareUploadResponse = serde_json::from_slice(&reply.body).unwrap();
    let token = session.files["f1"].clone();
    (session, token)
}

fn unverified(save: &Path) -> PathBuf {
    save.join("Received (unverified)")
}

#[test]
fn a_preexisting_partial_is_preserved_and_not_adopted() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, save, state) = start(&identity, Policy::default());
    let port = service.bound_port();
    let dir = unverified(&save);
    std::fs::create_dir_all(&dir).unwrap();
    // A crash leftover, and a user's own file that happens to have the partial's name.
    let leftover = dir.join(".hello.txt.test-part");
    std::fs::write(&leftover, b"OLD LEFTOVER BYTES THAT MUST SURVIVE").unwrap();
    let second = dir.join(".hello.txt.1.test-part");
    std::fs::write(&second, b"USER FILE").unwrap();

    let (session, token) = accepted_session(&service, port, "hello.txt", 5);
    assert_eq!(upload(port, &session, &token, b"hello"), 200);

    assert_eq!(std::fs::read(&leftover).unwrap(), b"OLD LEFTOVER BYTES THAT MUST SURVIVE");
    assert_eq!(std::fs::read(&second).unwrap(), b"USER FILE");
    // The received file holds only what was sent, not the leftover's bytes.
    assert_eq!(std::fs::read(dir.join("hello.txt")).unwrap(), b"hello");
    let _ = std::fs::remove_dir_all(&save);
    let _ = std::fs::remove_dir_all(&state);
}

#[test]
fn an_existing_received_file_is_never_overwritten() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, save, state) = start(&identity, Policy::default());
    let port = service.bound_port();
    let dir = unverified(&save);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hello.txt"), b"mine").unwrap();

    let (session, token) = accepted_session(&service, port, "hello.txt", 5);
    assert_eq!(upload(port, &session, &token, b"hello"), 200);
    assert_eq!(std::fs::read(dir.join("hello.txt")).unwrap(), b"mine");
    assert_eq!(std::fs::read(dir.join("hello (1).txt")).unwrap(), b"hello");
    let _ = std::fs::remove_dir_all(&save);
    let _ = std::fs::remove_dir_all(&state);
}

/// A service sharing an existing save folder (a second receiver on the same machine).
fn start_in(save: &Path, identity: &Identity) -> (Arc<Service>, PathBuf) {
    let state = temp("state");
    let service = Service::start(
        config(save, &state.join("nearby_devices.json"), Policy::default()),
        identity.clone(),
        Arc::new(|_: Event| {}),
    )
    .expect("service starts");
    (Arc::new(service), state)
}

#[test]
fn concurrent_transfers_of_the_same_name_each_keep_their_own_bytes() {
    // One service takes one transfer at a time (409), so concurrency means two receivers
    // writing into the same folder.
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service_a, save, state_a) = start(&identity, Policy::default());
    let (service_b, state_b) = start_in(&save, &identity);
    let (pa, pb) = (service_a.bound_port(), service_b.bound_port());
    let (s1, t1) = accepted_session(&service_a, pa, "same.bin", 600_000);
    let (s2, t2) = accepted_session(&service_b, pb, "same.bin", 600_000);
    let (a, b) = (vec![0xAAu8; 600_000], vec![0xBBu8; 600_000]);
    let (a2, b2) = (a.clone(), b.clone());
    let h1 = std::thread::spawn(move || upload(pa, &s1, &t1, &a2));
    let h2 = std::thread::spawn(move || upload(pb, &s2, &t2, &b2));
    assert_eq!(h1.join().unwrap(), 200);
    assert_eq!(h2.join().unwrap(), 200);

    let dir = unverified(&save);
    let mut got = vec![
        std::fs::read(dir.join("same.bin")).unwrap(),
        std::fs::read(dir.join("same (1).bin")).unwrap(),
    ];
    got.sort();
    assert_eq!(got, vec![a, b], "each transfer's bytes land intact in its own file");
    let stray: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".test-part"))
        .collect();
    assert!(stray.is_empty(), "stray partials: {stray:?}");
    let _ = std::fs::remove_dir_all(&save);
    let _ = std::fs::remove_dir_all(&state_a);
    let _ = std::fs::remove_dir_all(&state_b);
}

#[test]
fn cancelling_a_receive_removes_only_its_own_partial() {
    let _guard = serial();
    let identity = Identity::generate("Test Mac").unwrap();
    let (service, save, state) = start(&identity, Policy::default());
    let port = service.bound_port();
    let dir = unverified(&save);
    std::fs::create_dir_all(&dir).unwrap();
    let leftover = dir.join(".big.bin.test-part");
    std::fs::write(&leftover, b"LEFTOVER").unwrap();

    let (session, token) = accepted_session(&service, port, "big.bin", 400_000);
    let transfer_id = service.snapshot().transfers[0].id.clone();
    let target = format!(
        "{}/upload?sessionId={}&fileId=f1&token={}",
        proto::API,
        session.session_id,
        token
    );
    let mut wire = net::connect(loopback(), port, true, "", Duration::from_secs(30)).unwrap();
    let svc = service.clone();
    // The sender's write error after the server hangs up is expected.
    let _ = net::call(
        &mut wire,
        "POST",
        "127.0.0.1",
        &target,
        Some("application/octet-stream"),
        400_000,
        &mut |w| {
            w.write_all(&[1u8; 100_000])?;
            // Wait until the receiver has opened its partial and taken the bytes.
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline
                && std::fs::read_dir(&dir).map_or(true, |d| {
                    !d.filter_map(|e| e.ok()).any(|e| {
                        e.file_name().to_string_lossy() == ".big.bin.1.test-part"
                    })
                })
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(svc.cancel(&transfer_id));
            for _ in 0..3 {
                w.write_all(&[2u8; 100_000])?;
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        },
    );
    let dir = unverified(&save);
    // Give the receive thread a moment to clean up.
    for _ in 0..100 {
        if !dir.join(".big.bin.1.test-part").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(std::fs::read(&leftover).unwrap(), b"LEFTOVER");
    assert!(!dir.join(".big.bin.1.test-part").exists(), "our partial is removed");
    assert!(!dir.join("big.bin").exists(), "our reservation is removed");
    let _ = std::fs::remove_dir_all(&save);
    let _ = std::fs::remove_dir_all(&state);
}
