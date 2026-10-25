// rightkit-identity consumed by the exact registry pin in Cargo.toml, outside the workspace.
//
// Proves, with explicit clocks and injected liveness (no wall clock, no process probing):
//   * build_info: BuildIdentity::from_compile_time defaults every unset value to "unknown" and
//     reads the dirty flag from "1", "true", "yes" and "dirty" (trimmed) only; with_runtime_override
//     applies each of the five RIGHTKIT_* names and a present value wins; display() shortens the
//     revision to seven characters and marks a dirty build; the build_identity!() macro takes the
//     calling crate's own version; utc_stamp formats known Unix seconds; serde round trip.
//   * installation: machine_binding_hash is a 64-digit lowercase hex SHA-256 of a domain-separated,
//     trimmed input (checked against an independently computed value); host_display_name drops
//     control characters, collapses whitespace, strips .local and .lan, caps at 64 characters and
//     falls back to "This device"; InstallationStore mints a Fresh identity, Resumes it, reports
//     CloneDetected with the previous id when the machine binding changes (generation + 1); the
//     identity file is owner-only on Unix; claim_startup is Acquired, HeldByLiveProcess,
//     PreviousUncleanExit, advances the startup generation, and a rolled-back identity is reported
//     as CloneDetected with the colliding claim quarantined; release_claim and release_claim_nonce
//     remove a claim only for the right pid (and nonce); eight concurrent first runs agree on one id.
//   * pairing: the canonical transcript encoding ignores effect order and duplicates and changes
//     with any nonce; transcript_mac and confirmation_code match independently computed
//     HMAC-SHA256 values, roles are separated; PairingSession refuses a reflected MAC, approval
//     before confirmation, a grant wider than requested, use after completion, denial, or expiry,
//     and returns a DeviceGrant for the initiator device; ResumeTicket issue / verify / redeem
//     refuse a wrong key, a tampered device, an expired ticket and a replay; ReplayStore is
//     bounded, prunes expired ids and survives a serde snapshot.
//   * bearer: BearerRegistry mints "rkb_" plus 64 hex digits, keeps only hashes (a serialised
//     registry never contains the token and still authenticates after a round trip), a new mint
//     revokes the earlier token, revoke and active_devices; authenticate spends a rate-limit token
//     before comparing; RateLimiter is per IP, refills, shares a bucket per IPv6 /64 and per
//     IPv4-mapped address, and keeps an exhausted bucket under capacity churn.
// Public API only.
// Does not prove: build_script::emit (it belongs in a build.rs and runs git), the compile-time
// RIGHTKIT_* values a real build supplies, a real process-liveness probe, cross-process file
// locking, or key agreement (the caller supplies the shared secret).
// State: a per-process directory under the OS temp directory, removed at the end. No network, no
// process spawn, no wall clock, no real user data.
use rightkit_identity::build_info::{
    utc_stamp, ENV_CHANNEL, ENV_GIT_DIRTY, ENV_GIT_REV, ENV_PROFILE, ENV_UTC,
};
use rightkit_identity::{
    confirmation_code, host_display_name, machine_binding_hash, transcript_mac, AuthError,
    BearerRegistry, BuildIdentity, ClaimOutcome, DeviceGrant, Error, InstallationIdentity,
    InstallationStore, Observation, PairingSession, PairingState, PairingTranscript, RateLimiter,
    ReplayStore, ResumeTicket, Role, Startup,
};
use std::net::IpAddr;
use std::path::PathBuf;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("rk-identity-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create consumer temp dir");
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn ip(text: &str) -> IpAddr {
    text.parse().expect("fixture address")
}

fn refused(result: Result<impl std::fmt::Debug, Error>, why: &str) {
    match result {
        Err(Error::Refused(_)) => {}
        other => panic!("{why}: expected Refused, got {other:?}"),
    }
}

fn build_identity() {
    let unset = BuildIdentity::from_compile_time(None, None, None, None, None, "1.2.3");
    assert_eq!(unset.version, "1.2.3");
    assert_eq!(unset.git_rev, "unknown");
    assert_eq!(unset.channel, "unknown");
    assert_eq!(unset.built_utc, "unknown");
    assert!(!unset.dirty);
    assert!(
        unset.profile == "debug" || unset.profile == "release",
        "an unset profile falls back to the crate's own build profile: {}",
        unset.profile
    );

    for yes in ["1", "true", "yes", "dirty", " 1 "] {
        let b = BuildIdentity::from_compile_time(None, Some(yes), None, None, None, "1.0.0");
        assert!(b.dirty, "{yes:?} means dirty");
    }
    for no in ["0", "false", "no", "", "2", "TRUE"] {
        let b = BuildIdentity::from_compile_time(None, Some(no), None, None, None, "1.0.0");
        assert!(!b.dirty, "{no:?} does not mean dirty");
    }

    let full = BuildIdentity::from_compile_time(
        Some("0123456789abcdef"),
        Some("0"),
        Some("release"),
        Some("beta"),
        Some("2026-10-18T09:00:00Z"),
        "1.2.3",
    );
    assert_eq!(
        full.display(),
        "1.2.3+0123456 (release, beta, 2026-10-18T09:00:00Z)"
    );
    let dirty = BuildIdentity {
        dirty: true,
        ..full.clone()
    };
    assert_eq!(
        dirty.display(),
        "1.2.3+0123456-dirty (release, beta, 2026-10-18T09:00:00Z)"
    );
    let short = BuildIdentity {
        git_rev: "abc".to_owned(),
        ..full.clone()
    };
    assert!(short.display().starts_with("1.2.3+abc "));

    assert_eq!(ENV_GIT_REV, "RIGHTKIT_GIT_REV");
    assert_eq!(ENV_GIT_DIRTY, "RIGHTKIT_GIT_DIRTY");
    assert_eq!(ENV_PROFILE, "RIGHTKIT_BUILD_PROFILE");
    assert_eq!(ENV_CHANNEL, "RIGHTKIT_CHANNEL");
    assert_eq!(ENV_UTC, "RIGHTKIT_BUILD_UTC");
    let overridden = full.clone().with_runtime_override(|name| match name {
        "RIGHTKIT_GIT_REV" => Some("fedcba9876543210".to_owned()),
        "RIGHTKIT_GIT_DIRTY" => Some("yes".to_owned()),
        "RIGHTKIT_BUILD_PROFILE" => Some("debug".to_owned()),
        "RIGHTKIT_CHANNEL" => Some("stable".to_owned()),
        "RIGHTKIT_BUILD_UTC" => Some("2027-01-01T00:00:00Z".to_owned()),
        _ => None,
    });
    assert_eq!(
        overridden.display(),
        "1.2.3+fedcba9-dirty (debug, stable, 2027-01-01T00:00:00Z)"
    );
    assert_eq!(
        overridden.version, "1.2.3",
        "the version is never overridden"
    );
    let untouched = full.clone().with_runtime_override(|_| None);
    assert_eq!(untouched, full, "no override leaves the identity as it was");

    // The macro expands in this crate, so the version is this consumer's own.
    let built = rightkit_identity::build_identity!();
    assert_eq!(built.version, env!("CARGO_PKG_VERSION"));

    assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
    assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
    assert_eq!(utc_stamp(1_760_000_000), "2025-10-09T08:53:20Z");
    assert_eq!(utc_stamp(86_399), "1970-01-01T23:59:59Z");

    let json = serde_json::to_string(&full).expect("serialise");
    let back: BuildIdentity = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(back, full);
}

fn names_and_bindings() {
    assert_eq!(
        machine_binding_hash("machine-a"),
        "f24431f3b308653ec0ff519115c92add7320147c7afe784db9dabd1cd32cc114",
        "the binding is the domain-separated SHA-256 of the trimmed raw id"
    );
    assert_eq!(
        machine_binding_hash("  machine-a\n"),
        machine_binding_hash("machine-a"),
        "surrounding whitespace is trimmed"
    );
    assert_eq!(
        machine_binding_hash(""),
        "da4d348ef0ac4e59fee3f4353136a87630c62b7fc607ca34fc146b126fe653ab"
    );
    assert_ne!(
        machine_binding_hash("machine-a"),
        machine_binding_hash("machine-b")
    );
    let binding = machine_binding_hash("any");
    assert_eq!(binding.len(), 64);
    assert!(binding
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));

    assert_eq!(host_display_name("  Sam's\tMac.local "), "Sam's Mac");
    assert_eq!(host_display_name("desk.LAN"), "desk");
    assert_eq!(host_display_name("a   b\n c"), "a b c");
    assert_eq!(
        host_display_name("bell\u{7}ring"),
        "bellring",
        "control characters are dropped"
    );
    assert_eq!(host_display_name("\u{7}"), "This device");
    assert_eq!(host_display_name("   "), "This device");
    assert_eq!(host_display_name(".local"), "This device");
    assert_eq!(host_display_name(&"x".repeat(100)), "x".repeat(64));
}

fn installation_store() {
    let tmp = TempDir::new("store");
    let store = InstallationStore::new(tmp.0.join("data"));
    let a = machine_binding_hash("machine-a");
    let b = machine_binding_hash("machine-b");

    let first = store.load_or_create(&a, 10).expect("first run");
    let id = match &first {
        Startup::Fresh(id) => id.clone(),
        other => panic!("first run must be Fresh: {other:?}"),
    };
    assert_eq!(
        id.installation_id.len(),
        32,
        "a 16-byte id in lowercase hex"
    );
    assert!(id
        .installation_id
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_eq!(id.machine_binding, a);
    assert_eq!(id.created_unix, 10);
    assert_eq!(id.generation, 1);
    assert_eq!(id.startup_generation, 0);
    assert_eq!(first.identity(), &id);

    let identity_file = tmp.0.join("data").join("installation-identity.json");
    assert!(identity_file.is_file(), "the identity is persisted");
    let stored: InstallationIdentity =
        serde_json::from_slice(&std::fs::read(&identity_file).unwrap()).expect("identity JSON");
    assert_eq!(stored, id);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&identity_file)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "the identity file is owner-only: {mode:o}");
    }

    let again = store.load_or_create(&a, 20).expect("second run");
    assert!(matches!(again, Startup::Resumed(_)), "same binding resumes");
    assert_eq!(again.identity(), &id, "the stored creation time is kept");

    match store.load_or_create(&b, 30).expect("moved machine") {
        Startup::CloneDetected {
            previous_installation_id,
            previous_binding,
            identity,
        } => {
            assert_eq!(previous_installation_id, id.installation_id);
            assert_eq!(previous_binding, a);
            assert_ne!(
                identity.installation_id, id.installation_id,
                "a new id is minted"
            );
            assert_eq!(identity.machine_binding, b);
            assert_eq!(identity.generation, 2);
            assert_eq!(identity.created_unix, 30);
        }
        other => panic!("a changed binding must be CloneDetected: {other:?}"),
    }
    assert!(
        matches!(store.load_or_create(&b, 40).unwrap(), Startup::Resumed(_)),
        "the re-minted identity is now the stored one"
    );

    // A corrupt record is reported, never silently replaced.
    std::fs::write(&identity_file, b"{ not json").unwrap();
    assert!(matches!(
        store.load_or_create(&b, 50),
        Err(Error::Corrupt(_))
    ));
}

fn startup_claims() {
    let tmp = TempDir::new("claims");
    let store = InstallationStore::new(tmp.0.join("data"));

    // Without an identity on disk the claim still works and counts generations from zero.
    assert_eq!(
        store.claim_startup("i", 100, 1, |_| false).unwrap(),
        ClaimOutcome::Acquired
    );
    assert!(matches!(
        store.claim_startup("i", 200, 2, |_| true).unwrap(),
        ClaimOutcome::HeldByLiveProcess(claim) if claim.pid == 100
    ));
    assert!(matches!(
        store.claim_startup("i", 200, 3, |_| false).unwrap(),
        ClaimOutcome::PreviousUncleanExit(claim) if claim.pid == 100
    ));
    assert!(
        !store.release_claim(100).unwrap(),
        "only the claiming pid releases"
    );
    assert!(store.release_claim(200).unwrap());
    assert_eq!(
        store.read_claim().unwrap(),
        None,
        "a released claim is gone"
    );
    assert!(
        !store.release_claim(200).unwrap(),
        "releasing twice releases nothing"
    );

    // With an identity: each claim advances the append-only startup generation.
    let store = InstallationStore::new(tmp.0.join("with-identity"));
    let id = store
        .load_or_create(&machine_binding_hash("m"), 1)
        .unwrap()
        .identity()
        .clone();
    assert_eq!(
        store
            .claim_startup(&id.installation_id, 100, 2, |_| false)
            .unwrap(),
        ClaimOutcome::Acquired
    );
    let claim = store.read_claim().unwrap().expect("claim on disk");
    assert_eq!(claim.installation_id, id.installation_id);
    assert_eq!(claim.pid, 100);
    assert_eq!(claim.claimed_unix, 2);
    assert_eq!(claim.startup_generation, 1);
    assert_eq!(claim.nonce.len(), 32);
    assert!(!store.release_claim_nonce(100, "wrong-nonce").unwrap());
    assert!(!store.release_claim_nonce(999, &claim.nonce).unwrap());
    assert!(
        store.read_claim().unwrap().is_some(),
        "a stale holder removed nothing"
    );
    assert!(store.release_claim_nonce(100, &claim.nonce).unwrap());
    store
        .claim_startup(&id.installation_id, 101, 3, |_| false)
        .unwrap();
    assert_eq!(store.read_claim().unwrap().unwrap().startup_generation, 2);

    // A snapshot restore rolls the identity back while the newer claim survives: a collision.
    let dir = tmp.0.join("rollback");
    let store = InstallationStore::new(&dir);
    let id = store
        .load_or_create(&machine_binding_hash("m"), 1)
        .unwrap()
        .identity()
        .clone();
    store
        .claim_startup(&id.installation_id, 100, 2, |_| false)
        .unwrap();
    std::fs::write(
        dir.join("installation-identity.json"),
        serde_json::to_vec(&id).unwrap(),
    )
    .unwrap();
    match store
        .claim_startup(&id.installation_id, 200, 3, |_| false)
        .unwrap()
    {
        ClaimOutcome::CloneDetected {
            previous_installation_id,
            quarantined_claim,
            identity,
        } => {
            assert_eq!(previous_installation_id, id.installation_id);
            assert_eq!(quarantined_claim.pid, 100);
            assert_ne!(identity.installation_id, id.installation_id);
            assert_eq!(identity.generation, id.generation + 1);
            assert!(identity.startup_generation > quarantined_claim.startup_generation);
        }
        other => panic!("a rolled-back identity must be CloneDetected: {other:?}"),
    }
    let quarantined = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .any(|entry| entry.file_name().to_string_lossy().contains("quarantined"));
    assert!(
        quarantined,
        "the colliding claim is kept beside the identity"
    );
}

fn concurrent_first_run() {
    let tmp = TempDir::new("race");
    let dir = tmp.0.join("data");
    let binding = machine_binding_hash("m");
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (dir, binding) = (dir.clone(), binding.clone());
            std::thread::spawn(move || {
                InstallationStore::new(dir)
                    .load_or_create(&binding, 1)
                    .expect("load_or_create under contention")
                    .identity()
                    .installation_id
                    .clone()
            })
        })
        .collect();
    let ids: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(
        ids.windows(2).all(|w| w[0] == w[1]),
        "all first runs agree: {ids:?}"
    );
}

fn transcript() -> PairingTranscript {
    PairingTranscript {
        session_id: "s1".into(),
        initiator_device: "phone".into(),
        responder_device: "desktop".into(),
        initiator_nonce: vec![1; 16],
        responder_nonce: vec![2; 16],
        requested_effects: vec!["write".into(), "read".into(), "read".into()],
    }
}

fn pairing_encoding() {
    let base = transcript();
    let canonical = base.canonical();
    assert!(canonical.starts_with(b"rightkit-identity/transcript/v1"));
    assert_eq!(
        canonical.len(),
        233,
        "length-prefixed, tagged, order-fixed encoding"
    );

    let mut reordered = transcript();
    reordered.requested_effects = vec!["read".into(), "write".into()];
    assert_eq!(
        canonical,
        reordered.canonical(),
        "effect order and duplicates do not matter"
    );
    let mut other_nonce = transcript();
    other_nonce.responder_nonce = vec![3; 16];
    assert_ne!(canonical, other_nonce.canonical());
    let mut other_device = transcript();
    other_device.initiator_device = "tablet".into();
    assert_ne!(canonical, other_device.canonical());
    let mut other_session = transcript();
    other_session.session_id = "s2".into();
    assert_ne!(canonical, other_session.canonical());
    let mut fewer = transcript();
    fewer.requested_effects = vec!["read".into()];
    assert_ne!(canonical, fewer.canonical());

    // Independently computed HMAC-SHA256 values over the documented encoding.
    let secret = b"shared-secret";
    assert_eq!(
        hex(&transcript_mac(secret, Role::Initiator, &base)),
        "1a0c338f252d24cb51a2968a40c3dd155645c3792fe0133468e7812dde259949"
    );
    assert_eq!(
        hex(&transcript_mac(secret, Role::Responder, &base)),
        "00838d26bc30bd6d754e6e3aeb075a0fa2c789df6f4254f3c871edc4ad2989f9",
        "the two roles MAC under different labels, so a reflected MAC fails"
    );
    assert_eq!(confirmation_code(secret, &base), "812616");
    assert_eq!(confirmation_code(secret, &other_nonce), "744296");
    assert_eq!(confirmation_code(secret, &reordered), "812616");
    assert_ne!(
        transcript_mac(b"another-secret", Role::Initiator, &base),
        transcript_mac(secret, Role::Initiator, &base)
    );
    assert_eq!(Role::Initiator.peer(), Role::Responder);
    assert_eq!(Role::Responder.peer(), Role::Initiator);
}

fn pairing_flow() {
    let secret = b"shared-secret".to_vec();
    let mut desk = PairingSession::new(transcript(), secret.clone(), Role::Responder, 100, 60);
    let phone = PairingSession::new(transcript(), secret, Role::Initiator, 100, 60);
    assert_eq!(desk.state(), &PairingState::Proposed);
    assert_eq!(
        desk.code(),
        phone.code(),
        "both screens show the same six digits"
    );
    assert_eq!(desk.code().len(), 6);
    assert_eq!(desk.code(), "812616");
    assert_eq!(
        desk.local_mac(),
        transcript_mac(b"shared-secret", Role::Responder, &transcript())
    );

    refused(
        desk.owner_approve(vec!["read".to_string()], 101),
        "approval before confirmation",
    );
    refused(desk.confirm_peer(&desk.local_mac(), 101), "a reflected MAC");
    refused(desk.confirm_peer(&[0; 32], 101), "a wrong MAC");
    refused(desk.confirm_peer(&[], 101), "an empty MAC");
    assert_eq!(
        desk.state(),
        &PairingState::Proposed,
        "a wrong MAC leaves the state unchanged"
    );

    desk.confirm_peer(&phone.local_mac(), 101)
        .expect("the peer's MAC verifies");
    assert_eq!(desk.state(), &PairingState::TranscriptConfirmed);
    refused(
        desk.confirm_peer(&phone.local_mac(), 101),
        "confirming twice",
    );
    refused(
        desk.owner_approve(vec!["admin".to_string()], 102),
        "a grant wider than requested",
    );
    assert_eq!(
        desk.state(),
        &PairingState::TranscriptConfirmed,
        "a refused approval changes nothing"
    );
    refused(desk.complete(102), "completion before approval");

    desk.owner_approve(vec!["read".to_string()], 102)
        .expect("narrowed approval");
    assert_eq!(desk.state(), &PairingState::Approved);
    let grant = desk.complete(103).expect("approved session completes");
    assert_eq!(desk.state(), &PairingState::Complete);
    assert_eq!(
        grant.device_id, "phone",
        "the initiator device receives the grant"
    );
    assert!(grant.allows("read"));
    assert!(!grant.allows("write"), "the owner narrowed the request");
    assert_eq!(grant.effects.len(), 1);
    refused(desk.complete(104), "completing twice");
    refused(desk.owner_deny(104), "deciding a closed session");

    let json = serde_json::to_string(&grant).unwrap();
    let back: DeviceGrant = serde_json::from_str(&json).unwrap();
    assert_eq!(back, grant);

    // Expiry closes the session.
    let mut s = PairingSession::new(transcript(), b"k".to_vec(), Role::Responder, 0, 10);
    refused(
        s.confirm_peer(&[0; 32], 10),
        "the TTL boundary is already expired",
    );
    assert_eq!(s.state(), &PairingState::Expired);
    refused(
        s.confirm_peer(&[0; 32], 1),
        "an expired session stays closed",
    );
    let mut s = PairingSession::new(transcript(), b"k".to_vec(), Role::Responder, 0, 10);
    let peer = PairingSession::new(transcript(), b"k".to_vec(), Role::Initiator, 0, 10);
    s.confirm_peer(&peer.local_mac(), 9)
        .expect("one second before expiry");
    refused(
        s.owner_approve(vec!["read".to_string()], 11),
        "approval after expiry",
    );
    assert_eq!(s.state(), &PairingState::Expired);

    // Denial closes the session.
    let mut s = PairingSession::new(transcript(), b"k".to_vec(), Role::Responder, 0, 10);
    refused(s.owner_deny(1), "denial before confirmation");
    s.confirm_peer(&peer.local_mac(), 1).unwrap();
    s.owner_deny(2)
        .expect("the owner denies a confirmed transcript");
    assert_eq!(s.state(), &PairingState::Denied);
    refused(s.complete(3), "completing a denied session");
    refused(
        s.owner_approve(vec!["read".to_string()], 3),
        "approving a denied session",
    );

    // A grant may be empty: the owner can approve and grant nothing.
    let mut s = PairingSession::new(transcript(), b"k".to_vec(), Role::Responder, 0, 10);
    s.confirm_peer(&peer.local_mac(), 1).unwrap();
    s.owner_approve(Vec::<String>::new(), 2).unwrap();
    assert!(s.complete(3).unwrap().effects.is_empty());
}

fn resume_tickets_and_replay() {
    let key = b"ticket-key";
    let (ticket, token) = ResumeTicket::issue(key, "phone", 100, 50).expect("issue");
    assert_eq!(ticket.device_id, "phone");
    assert_eq!(ticket.expires_unix, 150);
    assert_eq!(ticket.ticket_id.len(), 32);
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(
        parts.len(),
        5,
        "v1.<id>.<expires>.<hex device>.<hex mac>: {token}"
    );
    assert_eq!(parts[0], "v1");
    assert_eq!(parts[1], ticket.ticket_id);
    assert_eq!(parts[2], "150");
    assert_eq!(parts[3], hex(b"phone"));
    assert_eq!(parts[4].len(), 64);

    let (other, other_token) = ResumeTicket::issue(key, "phone", 100, 50).unwrap();
    assert_ne!(
        other.ticket_id, ticket.ticket_id,
        "every ticket has its own id"
    );
    assert_ne!(other_token, token);

    assert_eq!(
        ResumeTicket::verify(key, &token, 110).expect("verifies"),
        ticket
    );
    assert_eq!(
        ResumeTicket::verify(key, &token, 149).expect("last second"),
        ticket
    );
    refused(
        ResumeTicket::verify(key, &token, 150),
        "at the expiry second",
    );
    refused(ResumeTicket::verify(b"other", &token, 110), "a wrong key");
    let tampered = token.replacen(&hex(b"phone"), &hex(b"other"), 1);
    refused(
        ResumeTicket::verify(key, &tampered, 110),
        "a tampered device",
    );
    let extended = token.replacen(".150.", ".999999.", 1);
    refused(
        ResumeTicket::verify(key, &extended, 110),
        "a tampered expiry",
    );
    for garbage in [
        "",
        "v1",
        "v2.a.1.aa.bb",
        "v1.a.1.aa",
        "v1.a.1.aa.zz",
        "v1.a.x.aa.00",
    ] {
        refused(ResumeTicket::verify(key, garbage, 0), "a malformed ticket");
    }

    let mut replay = ReplayStore::new(8);
    let redeemed = ResumeTicket::redeem(key, &token, 110, &mut replay).expect("first redemption");
    assert_eq!(redeemed.device_id, "phone");
    assert_eq!(replay.len(), 1);
    refused(
        ResumeTicket::redeem(key, &token, 111, &mut replay),
        "a replayed ticket",
    );
    ResumeTicket::redeem(key, &other_token, 111, &mut replay).expect("a different ticket redeems");
    assert_eq!(replay.len(), 2);

    let mut tiny = ReplayStore::new(1);
    ResumeTicket::redeem(key, &token, 110, &mut tiny).unwrap();
    refused(
        ResumeTicket::redeem(key, &other_token, 110, &mut tiny),
        "a full replay store fails closed",
    );

    let mut r = ReplayStore::new(2);
    assert!(r.is_empty());
    assert_eq!(r.observe("a", 0, 10), Observation::New);
    assert_eq!(r.observe("a", 1, 10), Observation::Duplicate);
    assert_eq!(r.observe("b", 1, 10), Observation::New);
    assert_eq!(r.observe("c", 2, 10), Observation::Full);
    assert_eq!(r.len(), 2);
    assert!(!r.is_empty());
    assert_eq!(
        r.observe("c", 11, 10),
        Observation::New,
        "expired ids are pruned first"
    );
    assert_eq!(
        r.observe("a", 11, 10),
        Observation::New,
        "an expired id may be seen again"
    );

    let json = serde_json::to_string(&r).unwrap();
    let mut back: ReplayStore = serde_json::from_str(&json).unwrap();
    assert_eq!(back.len(), r.len());
    assert_eq!(
        back.observe("c", 12, 10),
        Observation::Duplicate,
        "the snapshot keeps what was seen"
    );
}

fn bearer_tokens() {
    let mut registry = BearerRegistry::new();
    let mut limiter = RateLimiter::new(100, 1.0, 100);
    let addr = ip("10.0.0.1");

    let token = registry.mint("phone", 1).expect("mint");
    assert!(token.starts_with("rkb_"));
    assert_eq!(token.len(), 4 + 64);
    assert!(token[4..]
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    assert_eq!(registry.active_devices(), vec!["phone"]);
    assert_eq!(
        registry.authenticate(&token, addr, &mut limiter, 0),
        Ok("phone")
    );
    assert_eq!(
        registry.authenticate("rkb_nope", addr, &mut limiter, 0),
        Err(AuthError::Invalid)
    );
    assert_eq!(
        registry.authenticate("", addr, &mut limiter, 0),
        Err(AuthError::Invalid)
    );

    // The registry keeps only hashes.
    let snapshot = serde_json::to_string(&registry).unwrap();
    assert!(
        !snapshot.contains(&token),
        "the token is shown once and never stored"
    );
    assert!(!snapshot.contains("rkb_"));
    let restored: BearerRegistry = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(
        restored.authenticate(&token, addr, &mut limiter, 0),
        Ok("phone")
    );
    assert_eq!(restored.active_devices(), vec!["phone"]);

    // A new mint for the device revokes the earlier token.
    let second = registry.mint("phone", 2).unwrap();
    assert_ne!(second, token);
    assert_eq!(
        registry.authenticate(&token, addr, &mut limiter, 0),
        Err(AuthError::Invalid)
    );
    assert_eq!(
        registry.authenticate(&second, addr, &mut limiter, 0),
        Ok("phone")
    );
    assert_eq!(registry.active_devices(), vec!["phone"]);

    let tablet = registry.mint("tablet", 3).unwrap();
    assert_eq!(
        registry.authenticate(&tablet, addr, &mut limiter, 0),
        Ok("tablet")
    );
    assert_eq!(
        registry.authenticate(&second, addr, &mut limiter, 0),
        Ok("phone"),
        "devices are independent"
    );
    assert_eq!(registry.active_devices().len(), 2);
    assert!(registry.revoke("phone"));
    assert!(
        !registry.revoke("phone"),
        "nothing active is left to revoke"
    );
    assert!(!registry.revoke("unknown-device"));
    assert_eq!(
        registry.authenticate(&second, addr, &mut limiter, 0),
        Err(AuthError::Invalid)
    );
    assert_eq!(registry.active_devices(), vec!["tablet"]);

    assert_eq!(AuthError::Invalid.to_string(), "invalid bearer token");
    assert_eq!(
        AuthError::RateLimited {
            retry_after_ms: 1000
        }
        .to_string(),
        "rate limited; retry in 1000 ms"
    );
    let as_error: &dyn std::error::Error = &AuthError::Invalid;
    assert!(!as_error.to_string().is_empty());
}

fn rate_limits() {
    // Every attempt spends a token for its source IP before any comparison, valid token or not.
    let mut registry = BearerRegistry::new();
    let token = registry.mint("phone", 1).unwrap();
    let mut limiter = RateLimiter::new(2, 1.0, 100);
    let a = ip("192.0.2.1");
    assert_eq!(
        registry.authenticate("x", a, &mut limiter, 0),
        Err(AuthError::Invalid)
    );
    assert_eq!(
        registry.authenticate("x", a, &mut limiter, 0),
        Err(AuthError::Invalid)
    );
    assert_eq!(
        registry.authenticate(&token, a, &mut limiter, 0),
        Err(AuthError::RateLimited {
            retry_after_ms: 1000
        }),
        "a valid token does not bypass the limit"
    );
    assert_eq!(
        registry.authenticate(&token, ip("192.0.2.2"), &mut limiter, 0),
        Ok("phone"),
        "another address has its own bucket"
    );
    assert_eq!(
        registry.authenticate(&token, a, &mut limiter, 1000),
        Ok("phone"),
        "one second refills one token"
    );

    let mut limiter = RateLimiter::new(1, 0.001, 10);
    assert!(limiter.check(ip("::ffff:192.0.2.1"), 0).is_ok());
    assert!(
        limiter.check(ip("192.0.2.1"), 0).is_err(),
        "IPv4-mapped IPv6 shares the IPv4 bucket"
    );
    assert!(limiter.check(ip("::ffff:192.0.2.2"), 0).is_ok());

    let mut limiter = RateLimiter::new(1, 0.001, 10);
    assert!(limiter.check(ip("2001:db8::1"), 0).is_ok());
    assert!(
        limiter.check(ip("2001:db8::2"), 0).is_err(),
        "IPv6 shares a bucket per /64"
    );
    assert!(
        limiter.check(ip("2001:db8:0:1::1"), 0).is_ok(),
        "another /64 has its own"
    );

    // An exhausted bucket is never evicted to make room; new keys share one overflow bucket.
    let mut limiter = RateLimiter::new(1, 0.001, 1);
    assert!(limiter.check(ip("192.0.2.1"), 0).is_ok());
    assert!(
        limiter.check(ip("192.0.2.2"), 0).is_ok(),
        "the overflow bucket's one token"
    );
    assert!(limiter.check(ip("192.0.2.3"), 0).is_err());
    assert!(
        limiter.check(ip("192.0.2.1"), 0).is_err(),
        "no fresh burst for the first key"
    );
    assert!(
        limiter.check(ip("192.0.2.4"), 2_000_000).is_ok(),
        "an idle bucket refills and may be evicted"
    );

    // Out-of-range construction values are clamped, not rejected.
    let mut limiter = RateLimiter::new(0, 0.0, 0);
    assert!(
        limiter.check(ip("198.51.100.1"), 0).is_ok(),
        "capacity is at least one token"
    );
    assert!(limiter.check(ip("198.51.100.1"), 0).is_err());
    match limiter.check(ip("198.51.100.1"), 0) {
        Err(wait_ms) => assert!(wait_ms > 0),
        Ok(()) => panic!("the bucket is empty"),
    }
}

fn error_messages() {
    assert_eq!(
        Error::Random.to_string(),
        "operating system random source failed"
    );
    assert_eq!(Error::Io("disk".into()).to_string(), "io: disk");
    assert_eq!(
        Error::Corrupt("bad".into()).to_string(),
        "corrupt record: bad"
    );
    assert_eq!(Error::Refused("no").to_string(), "refused: no");
    let as_error: &dyn std::error::Error = &Error::Random;
    assert!(!as_error.to_string().is_empty());
}

fn main() {
    build_identity();
    names_and_bindings();
    installation_store();
    startup_claims();
    concurrent_first_run();
    pairing_encoding();
    pairing_flow();
    resume_tickets_and_replay();
    bearer_tokens();
    rate_limits();
    error_messages();
    println!("OK rightkit-identity (registry consumer)");
}
