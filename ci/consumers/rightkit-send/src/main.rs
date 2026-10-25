// rightkit-send 0.2.0 consumed by exact registry pin (default features), outside the workspace.
//
// Proves the receiving side end to end over loopback and the pure public surface, as a host that feeds
// discovery itself (the iOS configuration: no multicast, no Bonjour) uses it.
// Covered: Identity (generate, load, fingerprint binding, refusal of a mismatched or corrupt pair), Policy
// defaults and their effect (max_file_bytes, max_message_chars, answer_wait), Error (typed variants, stable
// codes, Display), TrustState and the trust store (default state, per-device Allow/Ask/Deny, labels, forget,
// persistence across a restart), the proto helpers (file-name sanitising, unique file reservation, mime,
// percent encoding, random ids, wire types), send::expand and Entry::open_reader, Service (start, bound_port,
// snapshot, status, devices, peer_add_resolved and peer_remove, send refusals that need no connection,
// stop and restart on the same port, PortInUse), and a real TLS exchange on 127.0.0.1 between the
// crate's sender (send::deliver) and a running Service: the owner's card and answer, accept, decline,
// Allow with certificate proof, Deny, a text message, a folder, the clipboard flag, a pinned-fingerprint
// mismatch and plaintext refusal.
// Does not prove: UDP multicast discovery and the network sweep (feature `multicast`), Bonjour (feature
// `bonjour`, macOS), the macOS quarantine mark on received files, Service::send's own connection to a
// listed peer (the host-fed list refuses loopback addresses, and no other address is contacted here),
// transfers of unproven senders (no certificate), and any real device on a real network.
// State: a per-process directory under the OS temp directory, removed at the end; TCP on 127.0.0.1 only
// (the services listen on an ephemeral port while they run). No network, no credentials, no user data.
use rightkit_send::proto::{self, DeviceInfo};
use rightkit_send::send::{self, Outcome, Phase, Progress};
use rightkit_send::trust::{self, normalize};
use rightkit_send::{
    net, Config, Entry, Error, Event, Identity, Incoming, Peer, Policy, SendItem, Service,
    Transfer, TrustState,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

// ---- helpers ---------------------------------------------------------------

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let root = std::env::temp_dir().join(format!("rk-send-consumer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create consumer temp dir");
        Scratch(root)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).expect("create scratch subdirectory");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(found) = probe() {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(15));
    }
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<Event>>>);

impl Log {
    fn handler(&self) -> Arc<dyn Fn(Event) + Send + Sync> {
        let events = self.0.clone();
        Arc::new(move |event| events.lock().unwrap().push(event))
    }

    fn all(&self) -> Vec<Event> {
        self.0.lock().unwrap().clone()
    }

    fn cards(&self) -> Vec<Incoming> {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                Event::Incoming(card) => Some(card),
                _ => None,
            })
            .collect()
    }

    fn resolved(&self) -> Vec<String> {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                Event::IncomingResolved(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    fn finished(&self) -> Vec<Transfer> {
        self.all()
            .into_iter()
            .filter_map(|e| match e {
                Event::Finished(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    fn nth_card(&self, n: usize) -> Incoming {
        wait_for("an incoming card", || self.cards().get(n).cloned())
    }

    fn nth_finished(&self, n: usize) -> Transfer {
        wait_for("a finished transfer", || self.finished().get(n).cloned())
    }
}

fn config(save: &Path, state: &Path, alias: &str, policy: Policy) -> Config {
    Config {
        product_name: "Fixture Product".into(),
        alias: alias.into(),
        port: 0,
        device_type: "desktop".into(),
        device_model: "Fixture Model".into(),
        save_dir: save.to_path_buf(),
        trust_path: state.join("nearby_devices.json"),
        user_agent: "FixtureAgent/1".into(),
        quarantine_agent: "FixtureAgent".into(),
        temp_suffix: ".fixture-part".into(),
        log_prefix: "fixture sharing:".into(),
        policy,
    }
}

fn loopback() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

fn device_info(identity: &Identity, alias: &str) -> DeviceInfo {
    DeviceInfo {
        alias: alias.into(),
        version: proto::VERSION.into(),
        device_model: Some("Fixture Phone".into()),
        device_type: Some("mobile".into()),
        fingerprint: identity.fingerprint.clone(),
        port: proto::PORT,
        protocol: "https".into(),
        download: false,
        announce: None,
    }
}

/// One `send::deliver` run, the crate's own sender, on a thread of its own with this identity presented as
/// the client certificate.
struct Delivery {
    handle: thread::JoinHandle<(Result<Outcome, Error>, Vec<Progress>)>,
}

impl Delivery {
    fn join(self) -> (Result<Outcome, Error>, Vec<Progress>) {
        self.handle.join().expect("sender thread")
    }
}

fn deliver_to(sender: &Identity, peer: Peer, items: Vec<SendItem>, clipboard: bool) -> Delivery {
    // The client identity is process-wide: set it just before the sender connects.
    net::set_client_identity(sender);
    let me = device_info(sender, "Fixture Sender");
    let handle = thread::spawn(move || {
        let entries = send::expand(&items).expect("expand the items");
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let result = send::deliver(
            &me,
            &peer,
            &entries,
            clipboard,
            &cancel,
            &|_| {},
            &mut |p| seen.push(p),
        );
        (result, seen)
    });
    Delivery { handle }
}

fn peer_for(service: &Service, fingerprint: &str, alias: &str) -> Peer {
    Peer {
        ip: loopback(),
        port: service.bound_port(),
        https: true,
        fingerprint: fingerprint.to_string(),
        alias: alias.to_string(),
    }
}

fn files_below(base: &Path) -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(listing) = fs::read_dir(dir) else {
            return;
        };
        for child in listing.flatten() {
            let name = format!("{prefix}{}", child.file_name().to_string_lossy());
            if child.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                walk(&child.path(), &format!("{name}/"), out);
            } else {
                out.push(name);
            }
        }
    }
    let mut out = Vec::new();
    walk(base, "", &mut out);
    out.sort();
    out
}

// ---- pure surface ----------------------------------------------------------

fn constants_policy_and_pins() {
    assert_eq!(proto::PORT, 53317, "LocalSend v2 default port");
    assert_eq!(proto::API, "/api/localsend/v2");
    assert_eq!(proto::VERSION, "2.0");
    assert_eq!(proto::MULTICAST_GROUP.octets(), [224, 0, 0, 167]);

    let policy = Policy::default();
    assert_eq!(policy.unverified_folder, "Received (unverified)");
    assert_eq!(policy.unproven_max_files, 200);
    assert_eq!(policy.unproven_max_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(policy.max_file_bytes, 64 * 1024 * 1024 * 1024);
    assert_eq!(policy.max_message_chars, 64 * 1024);
    assert_eq!(policy.answer_wait, Duration::from_secs(60));
    assert_eq!(policy.clone(), policy, "Policy is a comparable value");

    assert_eq!(
        net::sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(net::valid_pin(&"a".repeat(64)));
    assert!(
        net::valid_pin(&"A1".repeat(32)),
        "upper-case hex is a valid pin"
    );
    assert!(!net::valid_pin(&"a".repeat(63)));
    assert!(!net::valid_pin(&"a".repeat(65)));
    assert!(!net::valid_pin(&format!("{}g", "a".repeat(63))));
    assert!(!net::valid_pin(""));
}

fn identity_cases() {
    let identity = Identity::generate("Fixture Mac").expect("generate an identity");
    assert!(net::valid_pin(&identity.fingerprint));
    assert_eq!(
        identity.fingerprint,
        identity.fingerprint.to_ascii_lowercase()
    );
    assert_eq!(
        identity.fingerprint,
        net::sha256_hex(&identity.cert_der),
        "the fingerprint is the SHA-256 of the certificate"
    );
    assert!(!identity.cert_der.is_empty() && !identity.key_der.is_empty());
    let other = Identity::generate("Fixture Mac").expect("generate a second identity");
    assert_ne!(
        identity.fingerprint, other.fingerprint,
        "every identity is new"
    );

    // A stored pair loads back to the same fingerprint, which is what lets other devices remember it.
    let restored = Identity::load(identity.cert_der.clone(), identity.key_der.clone())
        .expect("load the stored pair");
    assert_eq!(restored.fingerprint, identity.fingerprint);
    assert_eq!(restored.cert_der, identity.cert_der);
    assert_eq!(restored.key_der, identity.key_der);

    // A key that does not belong to the certificate, and corrupt bytes, are refused.
    assert!(
        Identity::load(identity.cert_der.clone(), other.key_der.clone()).is_err(),
        "a certificate with another identity's key is refused"
    );
    assert!(
        Identity::load(vec![1, 2, 3], vec![4, 5, 6]).is_err(),
        "corrupt bytes are refused"
    );
    assert!(
        Identity::load(Vec::new(), Vec::new()).is_err(),
        "empty bytes are refused"
    );

    // The key never reaches a log line.
    let shown = format!("{identity:?}");
    assert!(shown.contains(&identity.fingerprint), "{shown}");
    assert!(
        !shown.contains("key_der") && !shown.contains("cert_der"),
        "{shown}"
    );

    // Odd aliases are cleaned, not refused.
    Identity::generate("tab\there\nand a very long alias ".repeat(8).as_str())
        .expect("control characters and length are tolerated");
}

fn errors_and_trust_types() {
    let mismatch = Error::PeerIdentityMismatch {
        peer: "Phone".into(),
    };
    let value = serde_json::to_value(&mismatch).unwrap();
    assert_eq!(value["code"], "peer_identity_mismatch");
    assert_eq!(value["peer"], "Phone");
    assert_eq!(
        serde_json::to_value(Error::InvalidFingerprint).unwrap(),
        json!({"code": "invalid_fingerprint"})
    );
    assert_eq!(
        serde_json::to_value(Error::PortInUse { port: 53317 }).unwrap(),
        json!({"code": "port_in_use", "port": 53317})
    );
    assert_eq!(
        serde_json::to_value(Error::Unverified {
            max_files: 2,
            max_bytes: 9
        })
        .unwrap(),
        json!({"code": "unverified", "max_files": 2, "max_bytes": 9})
    );
    assert_eq!(
        Error::PortInUse { port: 53317 }.to_string(),
        "port 53317 is already in use"
    );
    assert_eq!(Error::NothingToSend.to_string(), "nothing to send");
    assert_eq!(
        Error::DeviceNotNearby.to_string(),
        "device is not on the network"
    );
    assert_eq!(
        Error::PeerAnswer {
            peer: "Phone".into(),
            status: 418
        }
        .to_string(),
        "Phone answered 418"
    );
    assert_eq!(mismatch.clone(), mismatch, "Error is a comparable value");
    assert_ne!(mismatch, Error::NothingToSend);
    let as_dyn: &dyn std::error::Error = &mismatch;
    assert!(!as_dyn.to_string().is_empty());

    for state in [TrustState::Allow, TrustState::Ask, TrustState::Deny] {
        assert_eq!(
            TrustState::parse(state.as_str()),
            Some(state),
            "{state:?} round trip"
        );
        assert_eq!(
            serde_json::to_string(&state).unwrap(),
            format!("\"{}\"", state.as_str())
        );
    }
    assert_eq!(TrustState::Allow.as_str(), "allow");
    assert_eq!(TrustState::parse("ALLOW"), None, "parsing is exact");
    assert_eq!(TrustState::parse(""), None);
    assert_eq!(normalize("  AbC123  "), "abc123");
    assert_eq!(trust::PROOF_CERTIFICATE, "certificate");
    assert_eq!(trust::PROOF_CALLBACK, "callback");
    assert_eq!(trust::MAX_UNDECIDED, 200);
}

fn protocol_helpers(scratch: &Scratch) {
    // File names from a sender are made safe.
    assert_eq!(
        proto::sanitize_relative("../../etc/passwd"),
        vec!["etc", "passwd"]
    );
    assert_eq!(proto::sanitize_relative("..\\..\\x.txt"), vec!["x.txt"]);
    assert_eq!(proto::sanitize_relative("/abs/olute"), vec!["abs", "olute"]);
    assert_eq!(proto::sanitize_relative(".."), vec!["file"]);
    assert_eq!(proto::sanitize_relative(""), vec!["file"]);
    assert_eq!(proto::sanitize_relative("a//b/./c"), vec!["a", "b", "c"]);
    assert_eq!(
        proto::sanitize_relative("1/2/3/4/5/6/7/8/9/10"),
        vec!["3", "4", "5", "6", "7", "8", "9", "10"],
        "only the last eight components are kept"
    );
    assert_eq!(proto::sanitize_component("a/b"), "a_b");
    assert_eq!(proto::sanitize_component("C:evil"), "C_evil");
    assert_eq!(proto::sanitize_component(" ... "), "file");
    assert_eq!(
        proto::sanitize_component("a\u{7}b"),
        "a_b",
        "control characters are replaced"
    );
    assert_eq!(proto::sanitize_component("what?*|.txt"), "what___.txt");
    let long = format!("{}.jpeg", "a".repeat(400));
    let clean = proto::sanitize_component(&long);
    assert!(
        clean.len() <= 200 && clean.ends_with(".jpeg"),
        "long names keep their extension"
    );

    // Unique reservation never overwrites and never leaves the base folder.
    let base = scratch.dir("reserve");
    let parts = |s: &str| proto::sanitize_relative(s);
    let first = proto::reserve_unique(&base, &parts("hello.txt")).unwrap();
    let second = proto::reserve_unique(&base, &parts("hello.txt")).unwrap();
    let third = proto::reserve_unique(&base, &parts("hello.txt")).unwrap();
    assert_eq!(first, base.join("hello.txt"));
    assert_eq!(second, base.join("hello (1).txt"));
    assert_eq!(third, base.join("hello (2).txt"));
    assert!(
        first.is_file() && second.is_file(),
        "the reservation creates the empty file"
    );
    let nested = proto::reserve_unique(&base, &parts("dir/sub/leaf")).unwrap();
    assert_eq!(nested, base.join("dir").join("sub").join("leaf"));
    let escaped = proto::reserve_unique(&base, &parts("../../outside.txt")).unwrap();
    assert!(
        escaped.starts_with(&base),
        "traversal cannot leave the base folder: {escaped:?}"
    );
    assert!(
        proto::reserve_unique(&base, &[]).is_err(),
        "an empty name is refused"
    );

    // Mime types, ids and percent encoding.
    for (name, mime) in [
        ("a.JPG", "image/jpeg"),
        ("a.png", "image/png"),
        ("a.mp4", "video/mp4"),
        ("a.mp3", "audio/mpeg"),
        ("a.pdf", "application/pdf"),
        ("a.zip", "application/zip"),
        ("a.json", "application/json"),
        ("a.txt", "text/plain"),
        ("a.md", "text/plain"),
        ("a.html", "text/html"),
        ("a.csv", "text/csv"),
        ("a.unknown", "application/octet-stream"),
        ("noextension", "application/octet-stream"),
    ] {
        assert_eq!(proto::mime_for(name), mime, "{name}");
    }
    let id = proto::random_hex(16);
    assert_eq!(id.len(), 32);
    assert!(id
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_ne!(id, proto::random_hex(16), "ids are random");
    assert_eq!(proto::random_hex(0), "");
    assert_eq!(proto::url_encode("a b/c%d~-_."), "a%20b%2Fc%25d~-_.");
    assert_eq!(
        proto::url_decode(&proto::url_encode("a b/c%d\u{e9}")),
        "a b/c%d\u{e9}"
    );
    assert_eq!(proto::url_decode("a+b"), "a b");
    assert_eq!(proto::url_decode("100%"), "100%");

    // Wire types: camelCase JSON, protocol defaults when a field is absent.
    let minimal: DeviceInfo = serde_json::from_value(json!({"alias": "Phone"})).unwrap();
    assert_eq!(minimal.version, "2.0");
    assert_eq!(minimal.port, 53317);
    assert_eq!(minimal.protocol, "https");
    assert!(!minimal.download && minimal.announce.is_none() && minimal.fingerprint.is_empty());
    let sent = serde_json::to_value(DeviceInfo {
        device_model: Some("Model".into()),
        device_type: Some("mobile".into()),
        ..minimal
    })
    .unwrap();
    assert_eq!(sent["deviceModel"], "Model");
    assert_eq!(sent["deviceType"], "mobile");
    assert!(
        sent.get("announce").is_none(),
        "an absent announce flag is not sent"
    );
    let request: proto::PrepareUploadRequest = serde_json::from_value(json!({
        "info": {"alias": "Phone"},
        "files": {"f1": {"id": "f1", "fileName": "a.txt", "size": 3, "fileType": "text/plain"}}
    }))
    .unwrap();
    assert!(request.pulse.is_none());
    assert!(!proto::PulseExtra::default().clipboard);
    assert!(
        !proto::is_message(&request.files),
        "a file without a preview is not a message"
    );
    let mut as_message = request.files.clone();
    as_message.get_mut("f1").unwrap().preview = Some("hi".into());
    assert!(proto::is_message(&as_message));
    as_message.get_mut("f1").unwrap().preview = Some(String::new());
    assert!(
        !proto::is_message(&as_message),
        "an empty preview is not a message"
    );
    let response = proto::PrepareUploadResponse {
        session_id: "s".into(),
        files: BTreeMap::from([("f1".to_string(), "t".to_string())]),
    };
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({"sessionId": "s", "files": {"f1": "t"}})
    );
}

fn expand_and_entries(scratch: &Scratch) {
    let base = scratch.dir("expand");
    let file = base.join("report.txt");
    fs::write(&file, b"quarterly").unwrap();
    let entries = send::expand(&[SendItem::Path(file.clone())]).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        (entries[0].name.as_str(), entries[0].size),
        ("report.txt", 9)
    );
    assert_eq!(entries[0].mime, "text/plain");
    assert_eq!(entries[0].id, "f0");
    let read = |entry: &Entry| {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut entry.open_reader().unwrap(), &mut text).unwrap();
        text
    };
    assert_eq!(read(&entries[0]), "quarterly");
    assert_eq!(
        read(&entries[0]),
        "quarterly",
        "every call opens a fresh reader at the start"
    );
    fs::write(&file, b"changed later").unwrap();
    assert_eq!(read(&entries[0]), "changed later", "nothing is cached");
    fs::remove_file(&file).unwrap();
    assert!(
        entries[0].open_reader().is_err(),
        "a file that went away is an I/O error"
    );

    // Text becomes a text/plain entry named like <8 hex>.txt; empty text is dropped.
    let mixed = send::expand(&[
        SendItem::Text(String::new()),
        SendItem::Text("hello \u{2603}".into()),
        SendItem::Text("second".into()),
    ])
    .unwrap();
    assert_eq!(mixed.len(), 2, "empty text is skipped");
    assert_eq!((mixed[0].id.as_str(), mixed[1].id.as_str()), ("f0", "f1"));
    assert_eq!(
        mixed[0].size,
        "hello \u{2603}".len() as u64,
        "size is in bytes"
    );
    assert_eq!(mixed[0].mime, "text/plain");
    assert!(
        mixed[0].name.len() == 12 && mixed[0].name.ends_with(".txt"),
        "name {:?}",
        mixed[0].name
    );
    assert_eq!(read(&mixed[0]), "hello \u{2603}");

    // A folder keeps its structure; the walk order is not promised, so compare sorted.
    let folder = base.join("photos");
    fs::create_dir_all(folder.join("sub")).unwrap();
    fs::write(folder.join("a.txt"), b"a").unwrap();
    fs::write(folder.join("sub").join("b.png"), b"bb").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(folder.join("a.txt"), folder.join("link.txt")).unwrap();
    let walked = send::expand(&[SendItem::Path(folder)]).unwrap();
    let mut names: Vec<(String, u64, String)> = walked
        .iter()
        .map(|e| (e.name.clone(), e.size, e.mime.clone()))
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            ("photos/a.txt".to_string(), 1, "text/plain".to_string()),
            ("photos/sub/b.png".to_string(), 2, "image/png".to_string()),
        ],
        "symbolic links inside a folder are skipped"
    );
    let mut ids: Vec<&str> = walked.iter().map(|e| e.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["f0", "f1"]);

    // Refusals.
    assert_eq!(send::expand(&[]).unwrap_err(), Error::NothingToSend);
    assert_eq!(
        send::expand(&[SendItem::Text(String::new())]).unwrap_err(),
        Error::NothingToSend
    );
    let missing = base.join("missing.bin");
    assert!(matches!(
        send::expand(&[SendItem::Path(missing)]).unwrap_err(),
        Error::Unreadable { .. }
    ));
}

// ---- one running service ---------------------------------------------------

fn service_state_and_trust(scratch: &Scratch) {
    let identity = Identity::generate("Fixture Receiver").unwrap();
    let (save, state) = (scratch.dir("svc-save"), scratch.dir("svc-state"));
    let log = Log::default();
    let service = Service::start(
        config(&save, &state, "Fixture Receiver", Policy::default()),
        identity.clone(),
        log.handler(),
    )
    .expect("service starts");

    let port = service.bound_port();
    assert_ne!(port, 0, "port 0 was replaced by the bound port");
    assert_eq!(service.fingerprint(), identity.fingerprint);
    assert_eq!(service.alias(), "Fixture Receiver");
    let snapshot = service.snapshot();
    assert_eq!(snapshot.product, "Fixture Product");
    assert_eq!(snapshot.alias, "Fixture Receiver");
    assert_eq!(snapshot.fingerprint, identity.fingerprint);
    assert_eq!(snapshot.port, port);
    assert_eq!(snapshot.save_dir, save.to_string_lossy());
    assert!(snapshot.devices.is_empty() && snapshot.incoming.is_empty());
    assert!(snapshot.transfers.is_empty() && snapshot.warnings.is_empty());
    assert_eq!(
        snapshot.local_network, "unknown",
        "no multicast send was attempted"
    );
    assert!(!snapshot.scanning);
    let status = service.status();
    assert_eq!(
        (
            status.devices,
            status.transfers_open,
            status.transfers_listed
        ),
        (0, 0, 0)
    );
    assert_eq!((status.pending_incoming, status.warnings), (0, 0));
    assert!(!status.scanning);
    let snapshot_json = serde_json::to_value(&snapshot).unwrap();
    for key in [
        "product",
        "alias",
        "fingerprint",
        "port",
        "saveDir",
        "devices",
        "incoming",
        "transfers",
        "warnings",
        "localNetwork",
        "scanning",
    ] {
        assert!(snapshot_json.get(key).is_some(), "snapshot JSON has {key}");
    }

    // The service answers a plain protocol request: /info over pinned TLS.
    let reply = net::request_json(
        loopback(),
        port,
        true,
        &identity.fingerprint,
        "GET",
        &format!("{}/info", proto::API),
        None,
        Duration::from_secs(10),
    )
    .expect("the service answers /info");
    assert_eq!(reply.status, 200);
    let info: Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(info["alias"], "Fixture Receiver");
    assert_eq!(info["fingerprint"], identity.fingerprint.as_str());
    assert_eq!(info["deviceType"], "desktop");

    // Trust: new devices start as Ask; Allow is only ever set per device.
    assert_eq!(service.default_state(), TrustState::Ask);
    assert_eq!(
        service.set_default_state(TrustState::Allow),
        Err(Error::AllowNotDefault)
    );
    assert_eq!(service.default_state(), TrustState::Ask);
    service.set_default_state(TrustState::Deny).unwrap();
    assert_eq!(service.default_state(), TrustState::Deny);
    service.set_default_state(TrustState::Ask).unwrap();

    let friend = "ab".repeat(32);
    let stranger = "cd".repeat(32);
    assert_eq!(
        service.set_trust("", TrustState::Allow),
        Err(Error::InvalidFingerprint)
    );
    assert_eq!(
        service.set_trust(&"a".repeat(129), TrustState::Allow),
        Err(Error::InvalidFingerprint)
    );
    service
        .set_trust(&friend.to_uppercase(), TrustState::Allow)
        .unwrap();
    assert_eq!(
        service.set_label(&stranger, "Nobody"),
        Err(Error::NotListed)
    );
    service.set_label(&friend, "  Kitchen laptop  ").unwrap();
    service.set_trust(&stranger, TrustState::Deny).unwrap();
    let records = service.devices_trust();
    assert_eq!(records.len(), 2);
    let record = records
        .iter()
        .find(|r| r.fingerprint == friend)
        .expect("the friend's record");
    assert_eq!(record.state, TrustState::Allow);
    assert_eq!(record.label, "Kitchen laptop", "labels are trimmed");
    assert!(record.decided, "an owner decision is marked");
    assert_eq!(record.proof, None, "a decision is not proof");
    assert_eq!(record.refused, 0);
    assert_eq!(
        records
            .iter()
            .find(|r| r.fingerprint == stranger)
            .unwrap()
            .state,
        TrustState::Deny
    );
    assert!(
        state.join("nearby_devices.json").is_file(),
        "the first change creates the trust file"
    );
    assert!(
        log.all()
            .iter()
            .filter(|e| matches!(e, Event::Changed))
            .count()
            >= 4,
        "every change raises Event::Changed"
    );

    // Listing a host-fed peer. Loopback addresses are never taken as peers (they are this device), and a
    // documentation-range address is only recorded: nothing is ever sent to it here.
    let host: IpAddr = "192.0.2.10".parse().unwrap();
    let txt = vec![
        ("px".to_string(), "1".to_string()),
        ("model".to_string(), "Fixture Phone".to_string()),
        ("type".to_string(), "mobile".to_string()),
    ];
    assert_eq!(
        service.peer_add_resolved("abc", "Phone", host, 53317, &txt),
        Err(Error::InvalidFingerprint)
    );
    let phone = "ef".repeat(32);
    service
        .peer_add_resolved(&phone.to_uppercase(), "Fixture Phone", host, 4000, &txt)
        .unwrap();
    service
        .peer_add_resolved(&"12".repeat(32), "Loopback", loopback(), 4001, &[])
        .unwrap();
    let devices = service.devices();
    assert_eq!(
        devices.len(),
        1,
        "the loopback address was not listed: {devices:?}"
    );
    let device = &devices[0];
    assert_eq!(
        device.fingerprint, phone,
        "fingerprints are stored lower-case"
    );
    assert_eq!(
        (device.alias.as_str(), device.ip.as_str(), device.port),
        ("Fixture Phone", "192.0.2.10", 4000)
    );
    assert_eq!(device.protocol, "https");
    assert_eq!(device.device_model.as_deref(), Some("Fixture Phone"));
    assert_eq!(device.device_type.as_deref(), Some("mobile"));
    assert!(device.px, "the px flag comes from the TXT record");
    assert_eq!(service.status().devices, 1);
    assert_eq!(
        service.snapshot().devices.len(),
        1,
        "the snapshot lists the same devices"
    );
    assert!(
        log.all().iter().any(|e| matches!(e, Event::Devices)),
        "a new device raises Event::Devices"
    );
    // A device on the network cannot be forgotten, one that is not listed can.
    assert_eq!(service.forget(&phone), Err(Error::DeviceNearby));
    service.forget(&stranger).unwrap();
    assert_eq!(
        service.devices_trust().len(),
        2,
        "the phone was observed, the stranger forgotten"
    );

    // Sends that are refused before any connection.
    let items = || vec![SendItem::Text("hello".into())];
    assert_eq!(
        service.send(&"99".repeat(32), items(), false),
        Err(Error::DeviceNotNearby)
    );
    service.set_trust(&phone, TrustState::Deny).unwrap();
    assert_eq!(
        service.send(&phone, items(), false),
        Err(Error::DeviceBlocked)
    );
    assert_eq!(
        service.send("Fixture Phone", items(), false),
        Err(Error::DeviceBlocked),
        "an alias also finds the device"
    );
    assert!(service.peer_remove(&phone));
    assert!(
        !service.peer_remove(&phone),
        "removing twice reports it was gone"
    );
    assert!(service.devices().is_empty());
    assert!(!service.cancel("no-such-transfer"));
    assert!(!service.respond("no-such-request", true));

    // The save folder can move.
    let moved = scratch.dir("svc-save-moved");
    service.set_save_dir(moved.clone());
    assert_eq!(service.snapshot().save_dir, moved.to_string_lossy());

    // The owner's decisions survive a restart: same trust file, same records.
    service.stop();
    drop(service);
    let again = Service::start(
        config(&save, &state, "Fixture Receiver", Policy::default()),
        identity.clone(),
        Arc::new(|_: Event| {}),
    )
    .expect("service restarts");
    let after = again.devices_trust();
    let friend_after = after
        .iter()
        .find(|r| r.fingerprint == friend)
        .expect("the friend persisted");
    assert_eq!(
        (friend_after.state, friend_after.label.as_str()),
        (TrustState::Allow, "Kitchen laptop")
    );
    assert_eq!(
        after
            .iter()
            .find(|r| r.fingerprint == phone)
            .map(|r| r.state),
        Some(TrustState::Deny)
    );
    assert!(
        after.iter().all(|r| r.fingerprint != stranger),
        "a forgotten record stays forgotten"
    );
    again.forget(&friend).unwrap();
    assert!(again
        .devices_trust()
        .iter()
        .all(|r| r.fingerprint != friend));
}

fn stop_restart_and_port_in_use(scratch: &Scratch) {
    let identity = Identity::generate("Fixture Port").unwrap();
    let (save, state) = (scratch.dir("port-save"), scratch.dir("port-state"));
    let first = Service::start(
        config(&save, &state, "Fixture Port", Policy::default()),
        identity.clone(),
        Arc::new(|_: Event| {}),
    )
    .unwrap();
    let port = first.bound_port();

    // A second service on the same port is a typed error, and so is a port another program holds.
    let mut same = config(&save, &state, "Fixture Port Two", Policy::default());
    same.port = port;
    assert!(matches!(
        Service::start(same, identity.clone(), Arc::new(|_: Event| {})),
        Err(Error::PortInUse { port: p }) if p == port
    ));
    let held = TcpListener::bind(("0.0.0.0", 0)).expect("hold a port");
    let held_port = held.local_addr().unwrap().port();
    let mut taken = config(&save, &state, "Fixture Port Three", Policy::default());
    taken.port = held_port;
    assert!(matches!(
        Service::start(taken, identity.clone(), Arc::new(|_: Event| {})),
        Err(Error::PortInUse { port: p }) if p == held_port
    ));
    drop(held);

    // Stop is prompt, frees the port at once, and a service can start on it again, repeatedly.
    let began = Instant::now();
    first.stop();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "stop took {:?}",
        began.elapsed()
    );
    first.stop();
    for round in 0..3 {
        let mut again = config(&save, &state, "Fixture Port Again", Policy::default());
        again.port = port;
        let service = Service::start(again, identity.clone(), Arc::new(|_: Event| {}))
            .unwrap_or_else(|e| panic!("round {round}: the port is not free after stop: {e:?}"));
        assert_eq!(service.bound_port(), port);
        let reply = net::request_json(
            loopback(),
            port,
            true,
            "",
            "GET",
            &format!("{}/info", proto::API),
            None,
            Duration::from_secs(10),
        )
        .expect("the restarted service answers");
        assert_eq!(reply.status, 200);
        service.stop();
    }

    // An unusable identity fails before anything is listened on.
    let broken = Identity {
        cert_der: vec![1, 2, 3],
        key_der: vec![4, 5, 6],
        fingerprint: "0".repeat(64),
    };
    assert!(matches!(
        Service::start(
            config(&save, &state, "Broken", Policy::default()),
            broken,
            Arc::new(|_: Event| {})
        ),
        Err(Error::Identity { .. })
    ));
}

// ---- real exchanges over loopback ------------------------------------------

fn exchanges(scratch: &Scratch) {
    let receiver_identity = Identity::generate("Fixture Receiver").unwrap();
    let sender = Identity::generate("Fixture Sender").unwrap();
    let (save, state, source) = (
        scratch.dir("ex-save"),
        scratch.dir("ex-state"),
        scratch.dir("ex-source"),
    );
    let log = Log::default();
    let service = Service::start(
        config(&save, &state, "Fixture Receiver", Policy::default()),
        receiver_identity.clone(),
        log.handler(),
    )
    .expect("receiver starts");
    let peer = || peer_for(&service, &receiver_identity.fingerprint, "Fixture Receiver");
    let report = source.join("report.txt");
    fs::write(&report, b"quarterly figures").unwrap();

    // 1. Ask: the owner sees a card, accepts, and the file lands in the save folder intact.
    let delivery = deliver_to(&sender, peer(), vec![SendItem::Path(report.clone())], false);
    let card = log.nth_card(0);
    assert_eq!(card.from, "Fixture Sender");
    assert_eq!(card.fingerprint, sender.fingerprint);
    assert_eq!(card.device_model.as_deref(), Some("Fixture Phone"));
    assert_eq!(card.ip, "127.0.0.1");
    assert!(
        card.verified,
        "the sender presented the certificate behind its fingerprint"
    );
    assert_eq!(
        (card.file_count, card.total_bytes, card.is_message),
        (1, 17, false)
    );
    assert!(card.preview.is_none());
    assert_eq!(card.files.len(), 1);
    assert_eq!(
        (card.files[0].name.as_str(), card.files[0].size),
        ("report.txt", 17)
    );
    let waiting = service.snapshot();
    assert_eq!(waiting.incoming.len(), 1, "the card is in the snapshot");
    assert_eq!(waiting.incoming[0].id, card.id);
    assert_eq!(service.status().pending_incoming, 1);
    assert!(service.respond(&card.id, true));
    let (outcome, progress) = delivery.join();
    assert_eq!(outcome.unwrap(), Outcome::Done);
    assert!(
        !service.respond(&card.id, true),
        "an answered request is gone"
    );
    assert_eq!(progress.first().map(|p| p.phase), Some(Phase::Waiting));
    assert!(progress.iter().any(|p| p.phase == Phase::Sending));
    let last = progress.last().unwrap();
    assert_eq!(
        (last.done, last.total, last.files_done, last.files_total),
        (17, 17, 1, 1)
    );
    assert_eq!(
        fs::read(save.join("report.txt")).unwrap(),
        b"quarterly figures"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(save.join("report.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644, "received files are never executable");
    }
    let received = log.nth_finished(0);
    assert_eq!(
        (received.direction.as_str(), received.state.as_str()),
        ("receive", "done")
    );
    assert_eq!(received.peer, "Fixture Sender");
    assert_eq!(received.peer_fingerprint, sender.fingerprint);
    assert_eq!((received.total_bytes, received.done_bytes), (17, 17));
    assert_eq!((received.files_total, received.files_done), (1, 1));
    assert_eq!(received.saved_files.len(), 1);
    assert!(received.saved_files[0].ends_with("report.txt"));
    assert!(received.saved_to.is_some() && received.error.is_none() && !received.is_open());
    assert_eq!(
        log.resolved(),
        vec![card.id.clone()],
        "the answered card is withdrawn"
    );
    assert!(service.snapshot().incoming.is_empty());
    let record = service
        .devices_trust()
        .into_iter()
        .find(|r| r.fingerprint == sender.fingerprint)
        .unwrap();
    assert_eq!(
        record.state,
        TrustState::Ask,
        "a sighting takes the default state"
    );
    assert_eq!(record.proof.as_deref(), Some(trust::PROOF_CERTIFICATE));
    assert_eq!(record.alias_seen, "Fixture Sender");

    // 2. Decline: nothing is saved and the sender is told.
    let before = files_below(&save);
    let delivery = deliver_to(&sender, peer(), vec![SendItem::Path(report.clone())], false);
    let card = log.nth_card(1);
    assert!(service.respond(&card.id, false));
    assert_eq!(delivery.join().0.unwrap(), Outcome::Declined);
    assert_eq!(
        files_below(&save),
        before,
        "a declined request saves nothing"
    );

    // 3. A text message waits for the owner like a file, is not shown first, and is kept in the transfer
    //    record, never on disk.
    let delivery = deliver_to(
        &sender,
        peer(),
        vec![SendItem::Text("meet at noon".into())],
        false,
    );
    let card = log.nth_card(2);
    assert!(card.is_message);
    assert!(
        card.preview.is_none(),
        "the text is not shown before the owner accepts"
    );
    assert!(card.files.is_empty());
    assert!(!serde_json::to_string(&card)
        .unwrap()
        .contains("meet at noon"));
    assert!(service.respond(&card.id, true));
    assert_eq!(delivery.join().0.unwrap(), Outcome::Done);
    let message = wait_for("the message transfer", || {
        service
            .snapshot()
            .transfers
            .into_iter()
            .find(|t| t.message.is_some())
    });
    assert_eq!(message.message.as_deref(), Some("meet at noon"));
    assert_eq!(message.state, "done");
    assert_eq!(
        files_below(&save),
        before,
        "a message is not written to disk"
    );

    // 4. Allow with certificate proof: no card, the file arrives, and a name that exists is never overwritten.
    service
        .set_trust(&sender.fingerprint, TrustState::Allow)
        .unwrap();
    let cards_before = log.cards().len();
    let delivery = deliver_to(&sender, peer(), vec![SendItem::Path(report.clone())], false);
    let (outcome, _) = delivery.join();
    assert_eq!(outcome.unwrap(), Outcome::Done);
    assert_eq!(log.cards().len(), cards_before, "Allow raises no card");
    assert_eq!(
        fs::read(save.join("report.txt")).unwrap(),
        b"quarterly figures"
    );
    assert_eq!(
        fs::read(save.join("report (1).txt")).unwrap(),
        b"quarterly figures",
        "an existing file is kept"
    );
    let record = service
        .devices_trust()
        .into_iter()
        .find(|r| r.fingerprint == sender.fingerprint)
        .unwrap();
    assert!(record.decided && record.state == TrustState::Allow);

    // 5. The clipboard flag is honoured only for an Allow sender with certificate proof.
    let delivery = deliver_to(&sender, peer(), vec![SendItem::Text("pasted".into())], true);
    assert_eq!(delivery.join().0.unwrap(), Outcome::Done);
    let clip = wait_for("the clipboard transfer", || {
        service
            .snapshot()
            .transfers
            .into_iter()
            .find(|t| t.message.as_deref() == Some("pasted"))
    });
    assert!(clip.clipboard, "the clipboard flag reached the receiver");
    let plain = wait_for("the message transfer", || {
        service
            .snapshot()
            .transfers
            .into_iter()
            .find(|t| t.message.as_deref() == Some("meet at noon"))
    });
    assert!(!plain.clipboard);

    // 6. A folder keeps its structure below the save folder.
    let folder = source.join("album");
    fs::create_dir_all(folder.join("inner")).unwrap();
    fs::write(folder.join("one.txt"), b"1").unwrap();
    fs::write(folder.join("inner").join("two.txt"), b"22").unwrap();
    let delivery = deliver_to(&sender, peer(), vec![SendItem::Path(folder)], false);
    assert_eq!(delivery.join().0.unwrap(), Outcome::Done);
    assert_eq!(fs::read(save.join("album").join("one.txt")).unwrap(), b"1");
    assert_eq!(
        fs::read(save.join("album").join("inner").join("two.txt")).unwrap(),
        b"22"
    );

    // 7. Deny: refused at once without a card, and counted.
    service
        .set_trust(&sender.fingerprint, TrustState::Deny)
        .unwrap();
    let cards_before = log.cards().len();
    let before = files_below(&save);
    let delivery = deliver_to(&sender, peer(), vec![SendItem::Path(report.clone())], false);
    assert_eq!(delivery.join().0.unwrap(), Outcome::Declined);
    assert_eq!(log.cards().len(), cards_before, "Deny raises no card");
    assert_eq!(files_below(&save), before);
    let record = service
        .devices_trust()
        .into_iter()
        .find(|r| r.fingerprint == sender.fingerprint)
        .unwrap();
    assert_eq!(record.state, TrustState::Deny);
    service
        .set_trust(&sender.fingerprint, TrustState::Ask)
        .unwrap();

    // 8. A transfer list entry can be dismissed once it has ended; the list keeps at most a dozen.
    let ended = service
        .snapshot()
        .transfers
        .into_iter()
        .find(|t| !t.is_open())
        .expect("an ended transfer");
    service.dismiss(&ended.id);
    assert!(service
        .snapshot()
        .transfers
        .iter()
        .all(|t| t.id != ended.id));

    // 9. Identity pinning. A peer whose certificate is not the one its fingerprint names is refused as a
    //    mismatch, not as an unreachable device; a peer on plain HTTP and a malformed pin are refused
    //    before any connection.
    let impostor = Identity::generate("Impostor").unwrap();
    let wrong = Peer {
        fingerprint: impostor.fingerprint.clone(),
        ..peer()
    };
    let (outcome, _) =
        deliver_to(&sender, wrong, vec![SendItem::Path(report.clone())], false).join();
    assert!(
        matches!(&outcome, Err(Error::PeerIdentityMismatch { peer }) if peer == "Fixture Receiver"),
        "{outcome:?}"
    );
    let plaintext = Peer {
        https: false,
        ..peer()
    };
    let (outcome, _) = deliver_to(
        &sender,
        plaintext,
        vec![SendItem::Path(report.clone())],
        false,
    )
    .join();
    assert!(
        matches!(outcome, Err(Error::PeerPlaintext { .. })),
        "{outcome:?}"
    );
    let malformed = Peer {
        fingerprint: "not-a-pin".into(),
        ..peer()
    };
    let (outcome, _) = deliver_to(
        &sender,
        malformed,
        vec![SendItem::Path(report.clone())],
        false,
    )
    .join();
    assert!(
        matches!(outcome, Err(Error::InvalidIdentity { .. })),
        "{outcome:?}"
    );

    // The sender's own connection failure is typed too: a closed loopback port.
    let closed = {
        let probe = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        probe.local_addr().unwrap().port()
    };
    let gone = Peer {
        port: closed,
        ..peer()
    };
    let (outcome, _) =
        deliver_to(&sender, gone, vec![SendItem::Path(report.clone())], false).join();
    assert!(
        matches!(&outcome, Err(Error::PeerUnreachable { peer, .. }) if peer == "Fixture Receiver"),
        "{outcome:?}"
    );
    service.stop();
}

fn policy_effects(scratch: &Scratch) {
    let sender = Identity::generate("Fixture Sender").unwrap();
    let receiver_identity = Identity::generate("Fixture Receiver").unwrap();
    let source = scratch.dir("policy-source");
    let big = source.join("big.bin");
    fs::write(&big, vec![7u8; 2000]).unwrap();

    // max_file_bytes: a file over the cap is refused before a card is raised.
    let (save, state) = (scratch.dir("policy-save-a"), scratch.dir("policy-state-a"));
    let log = Log::default();
    let capped = Service::start(
        config(
            &save,
            &state,
            "Capped",
            Policy {
                max_file_bytes: 1000,
                ..Policy::default()
            },
        ),
        receiver_identity.clone(),
        log.handler(),
    )
    .unwrap();
    let peer = peer_for(&capped, &receiver_identity.fingerprint, "Capped");
    let (outcome, _) = deliver_to(&sender, peer, vec![SendItem::Path(big.clone())], false).join();
    assert!(
        matches!(&outcome, Err(Error::PeerAnswer { status: 400, .. })),
        "{outcome:?}"
    );
    assert!(log.cards().is_empty() && files_below(&save).is_empty());
    capped.stop();

    // max_message_chars: a longer text is cut to the cap, counted in characters.
    let (save, state) = (scratch.dir("policy-save-b"), scratch.dir("policy-state-b"));
    let service = Service::start(
        config(
            &save,
            &state,
            "Short",
            Policy {
                max_message_chars: 5,
                ..Policy::default()
            },
        ),
        receiver_identity.clone(),
        Arc::new(|_: Event| {}),
    )
    .unwrap();
    service
        .set_trust(&sender.fingerprint, TrustState::Allow)
        .unwrap();
    let peer = peer_for(&service, &receiver_identity.fingerprint, "Short");
    let (outcome, _) = deliver_to(
        &sender,
        peer,
        vec![SendItem::Text("h\u{e9}llo world".into())],
        false,
    )
    .join();
    assert_eq!(outcome.unwrap(), Outcome::Done);
    let cut = wait_for("the cut message", || {
        service
            .snapshot()
            .transfers
            .into_iter()
            .find(|t| t.message.is_some())
    });
    assert_eq!(
        cut.message.as_deref(),
        Some("h\u{e9}llo"),
        "five characters, not five bytes"
    );
    service.stop();

    // answer_wait: a request nobody answers is declined when the wait ends, and its card is withdrawn.
    let (save, state) = (scratch.dir("policy-save-c"), scratch.dir("policy-state-c"));
    let log = Log::default();
    let service = Service::start(
        config(
            &save,
            &state,
            "Impatient",
            Policy {
                answer_wait: Duration::from_millis(300),
                ..Policy::default()
            },
        ),
        receiver_identity.clone(),
        log.handler(),
    )
    .unwrap();
    let peer = peer_for(&service, &receiver_identity.fingerprint, "Impatient");
    let began = Instant::now();
    let (outcome, _) = deliver_to(&sender, peer, vec![SendItem::Path(big)], false).join();
    assert_eq!(
        outcome.unwrap(),
        Outcome::Declined,
        "an unanswered request is declined"
    );
    assert!(
        began.elapsed() < Duration::from_secs(15),
        "took {:?}",
        began.elapsed()
    );
    assert_eq!(log.cards().len(), 1);
    assert_eq!(log.resolved().len(), 1, "the timed-out card is withdrawn");
    assert!(service.snapshot().incoming.is_empty() && files_below(&save).is_empty());
    service.stop();
}

fn main() {
    let scratch = Scratch::new();
    constants_policy_and_pins();
    identity_cases();
    errors_and_trust_types();
    protocol_helpers(&scratch);
    expand_and_entries(&scratch);
    service_state_and_trust(&scratch);
    stop_restart_and_port_in_use(&scratch);
    exchanges(&scratch);
    policy_effects(&scratch);
    println!("OK rightkit-send 0.2.0 (registry consumer)");
}
