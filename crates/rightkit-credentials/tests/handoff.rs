//! Owner-binary hand-off: codec, envelope, snapshot, delivery and owner policy, deterministic
//! on every OS.

use std::process::Command;

use rightkit_credentials::handoff::{
    expect_ack, expect_reply, read_message, read_single_message, receive_delivery,
    receive_snapshot_delivery, scrub_inherited_env, write_message, write_message_with_limits,
    Entry, ErrorCode, HandoffError, HandoffPolicy, Limits, Message, Mode, SecretBundle, Snapshot,
    SnapshotDelivery, HEADER_LEN, MAGIC, VERSION,
};
use rightkit_credentials::Secret;

const VALUE: &str = "sk-live-SUPERSECRET-0042";

fn bundle() -> SecretBundle {
    let mut b = SecretBundle::new();
    b.insert("provider.openai", Secret::from_string(VALUE))
        .unwrap();
    b.insert(
        "provider.deepgram",
        Secret::from_bytes(vec![1u8, 2, 3, 250]),
    )
    .unwrap();
    b
}

fn bytes_of(m: &Message) -> Vec<u8> {
    let mut v = Vec::new();
    write_message(&mut v, m).unwrap();
    v
}

fn reply_frame(id: u64) -> Vec<u8> {
    bytes_of(&Message::reply_bundle(id, bundle()))
}

/// A frame built by hand around an arbitrary body.
fn raw(kind: u8, id: u64, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&MAGIC);
    v.push(VERSION);
    v.push(kind);
    v.extend_from_slice(&id.to_be_bytes());
    v.extend_from_slice(&(body.len() as u32).to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn strict_reply_body(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut v = vec![0u8];
    v.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for (name, hex) in entries {
        v.push(name.len() as u8);
        v.extend_from_slice(name);
        v.push(0);
        v.extend_from_slice(&(hex.len() as u32).to_be_bytes());
        v.extend_from_slice(hex);
    }
    v
}

fn read(bytes: &[u8]) -> Result<Message, HandoffError> {
    read_message(&mut &bytes[..], &Limits::default())
}

#[test]
fn reply_round_trip_preserves_names_and_values() {
    let bytes = reply_frame(42);
    assert_eq!(&bytes[..4], b"RKHO");
    assert_eq!(bytes[4], 1);
    assert_eq!(bytes[5], 2);
    let msg = read_single_message(&mut bytes.as_slice(), &Limits::default()).unwrap();
    let snap = expect_reply(
        msg,
        42,
        Mode::Strict,
        &["provider.openai", "provider.deepgram"],
    )
    .unwrap();
    let got = snap.into_bundle().unwrap();
    assert_eq!(
        got.get("provider.openai").unwrap().expose_str().unwrap(),
        VALUE
    );
    assert_eq!(
        got.get("provider.deepgram").unwrap().expose_bytes(),
        &[1, 2, 3, 250]
    );
}

#[test]
fn the_wire_carries_hex_not_raw_value_bytes() {
    let hay = String::from_utf8_lossy(&reply_frame(1)).into_owned();
    assert!(!hay.contains(VALUE));
}

#[test]
fn wrong_magic_version_and_kind_are_refused() {
    let mut b = reply_frame(1);
    b[1] = b'Z';
    assert_eq!(read(&b).unwrap_err(), HandoffError::BadMagic);
    let mut b = reply_frame(1);
    b[4] = 9;
    assert_eq!(
        read(&b).unwrap_err(),
        HandoffError::UnsupportedVersion { found: 9 }
    );
    let mut b = reply_frame(1);
    b[5] = 77;
    assert_eq!(
        read(&b).unwrap_err(),
        HandoffError::UnknownKind { found: 77 }
    );
}

#[test]
fn oversize_is_refused_before_the_body_is_read() {
    let mut header = raw(2, 1, &[]);
    header[14..18].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(read(&header), Err(HandoffError::Oversize { .. })));
    let small = Limits {
        max_frame_bytes: 16,
        max_names: 32,
    };
    assert!(matches!(
        read_message(&mut reply_frame(1).as_slice(), &small),
        Err(HandoffError::Oversize { limit: 16 })
    ));
    let mut sink = Vec::new();
    assert!(matches!(
        write_message_with_limits(&mut sink, &Message::reply_bundle(1, bundle()), &small),
        Err(HandoffError::Oversize { .. })
    ));
    assert!(sink.is_empty());
}

#[test]
fn too_many_names_is_refused() {
    let few = Limits {
        max_frame_bytes: 64 * 1024,
        max_names: 1,
    };
    assert!(matches!(
        read_message(&mut reply_frame(1).as_slice(), &few),
        Err(HandoffError::TooManyNames { limit: 1 })
    ));
}

#[test]
fn trailing_bytes_are_refused_inside_and_after_the_frame() {
    let mut after = reply_frame(1);
    after.push(0);
    assert_eq!(
        read_single_message(&mut after.as_slice(), &Limits::default()).unwrap_err(),
        HandoffError::TrailingBytes
    );
    let mut body = strict_reply_body(&[(b"a", b"6162")]);
    body.push(0);
    assert_eq!(
        read(&raw(2, 1, &body)).unwrap_err(),
        HandoffError::TrailingBytes
    );
    // An Ack with a body is refused too.
    assert_eq!(
        read(&raw(4, 1, &[0])).unwrap_err(),
        HandoffError::TrailingBytes
    );
}

#[test]
fn concatenated_frames_are_read_one_at_a_time() {
    let mut stream = reply_frame(1);
    stream.extend_from_slice(&bytes_of(&Message::ack(2)));
    stream.extend_from_slice(&reply_frame(3));
    let mut r = stream.as_slice();
    let l = Limits::default();
    assert_eq!(read_message(&mut r, &l).unwrap().id(), 1);
    assert_eq!(read_message(&mut r, &l).unwrap().id(), 2);
    assert_eq!(read_message(&mut r, &l).unwrap().id(), 3);
    assert_eq!(
        read_message(&mut r, &l).unwrap_err(),
        HandoffError::Truncated
    );
}

#[test]
fn bad_and_duplicate_names_are_refused() {
    for name in [&b"Upper"[..], b"sp ace", b"slash/x", b"\xff\xfe"] {
        let body = strict_reply_body(&[(name, b"00")]);
        assert_eq!(
            read(&raw(2, 1, &body)).unwrap_err(),
            HandoffError::BadName,
            "{name:?}"
        );
    }
    let long = [b'a'; 65];
    let body = strict_reply_body(&[(&long, b"00")]);
    assert_eq!(read(&raw(2, 1, &body)).unwrap_err(), HandoffError::BadName);
    let body = strict_reply_body(&[(b"dup", b"6162"), (b"dup", b"6364")]);
    assert_eq!(
        read(&raw(2, 1, &body)).unwrap_err(),
        HandoffError::DuplicateName { name: "dup".into() }
    );
    let mut sb = SecretBundle::new();
    assert_eq!(
        sb.insert("Bad Name", Secret::from_string("x")).unwrap_err(),
        HandoffError::BadName
    );
    assert!(matches!(
        Message::request(1, Mode::Strict, &["k", "k"]),
        Err(HandoffError::DuplicateName { .. })
    ));
}

#[test]
fn truncated_frames_are_refused_at_every_length() {
    let bytes = reply_frame(5);
    assert!(bytes.len() > HEADER_LEN);
    for cut in 0..bytes.len() {
        assert_eq!(
            read(&bytes[..cut]).unwrap_err(),
            HandoffError::Truncated,
            "cut at {cut}"
        );
    }
}

#[test]
fn malformed_values_are_refused() {
    let odd = strict_reply_body(&[(b"a", b"616")]);
    assert!(matches!(
        read(&raw(2, 1, &odd)),
        Err(HandoffError::Malformed(_))
    ));
    let nonhex = strict_reply_body(&[(b"a", b"zz")]);
    assert!(matches!(
        read(&raw(2, 1, &nonhex)),
        Err(HandoffError::Malformed(_))
    ));
}

#[test]
fn debug_and_errors_never_print_values() {
    let shown = format!("{:?}", bundle());
    assert!(shown.contains("provider.openai") && !shown.contains("SUPERSECRET"));
    let shown = format!("{:?}", Message::reply_bundle(1, bundle()));
    assert!(shown.contains("provider.openai") && !shown.contains("SUPERSECRET"));
    let mut snap = Snapshot::new();
    snap.insert_present("k", Secret::from_string(VALUE))
        .unwrap();
    snap.insert_failed("f", ErrorCode::StoreError).unwrap();
    let shown = format!("{snap:?}");
    assert!(!shown.contains("SUPERSECRET") && shown.contains("ReadFailed"));
}

// ---- envelope ----

#[test]
fn request_round_trip() {
    let m = Message::request(9, Mode::Snapshot, &["b.key", "a.key"]).unwrap();
    let (id, mode, names) = read(&bytes_of(&m)).unwrap().into_request().unwrap();
    assert_eq!((id, mode), (9, Mode::Snapshot));
    assert_eq!(names, vec!["b.key".to_string(), "a.key".to_string()]);
    assert_eq!(
        read(&reply_frame(1)).unwrap().into_request().unwrap_err(),
        HandoffError::UnexpectedKind
    );
}

#[test]
fn reply_id_mismatch_and_stale_reply_are_rejected() {
    let names = ["provider.openai", "provider.deepgram"];
    let stale = read(&reply_frame(1)).unwrap();
    assert_eq!(
        expect_reply(stale, 2, Mode::Strict, &names).unwrap_err(),
        HandoffError::IdMismatch {
            expected: 2,
            found: 1
        }
    );
    // An error for an earlier request is stale as well.
    let stale_err = read(&bytes_of(&Message::error(
        1,
        ErrorCode::NotFound,
        Some("x"),
    )))
    .unwrap();
    assert_eq!(
        expect_reply(stale_err, 2, Mode::Strict, &names).unwrap_err(),
        HandoffError::IdMismatch {
            expected: 2,
            found: 1
        }
    );
}

#[test]
fn reply_with_wrong_or_extra_or_missing_names_is_rejected() {
    let msg = || read(&reply_frame(3)).unwrap();
    assert_eq!(
        expect_reply(msg(), 3, Mode::Strict, &["provider.openai"]).unwrap_err(),
        HandoffError::NamesMismatch
    );
    assert_eq!(
        expect_reply(
            msg(),
            3,
            Mode::Strict,
            &["provider.openai", "provider.other"]
        )
        .unwrap_err(),
        HandoffError::NamesMismatch
    );
    assert_eq!(
        expect_reply(
            msg(),
            3,
            Mode::Snapshot,
            &["provider.openai", "provider.deepgram"]
        )
        .unwrap_err(),
        HandoffError::ModeMismatch
    );
}

#[test]
fn error_message_round_trip_carries_a_code_and_no_value() {
    for (code, name) in [
        (ErrorCode::NotAllowed, Some("provider.openai")),
        (ErrorCode::NotFound, Some("provider.openai")),
        (ErrorCode::StoreError, None),
        (ErrorCode::Invalid, None),
    ] {
        let msg = read(&bytes_of(&Message::error(11, code, name))).unwrap();
        assert_eq!(
            expect_reply(msg, 11, Mode::Strict, &["provider.openai"]).unwrap_err(),
            HandoffError::Remote {
                code,
                name: name.map(str::to_owned)
            }
        );
    }
    // A name that is not valid is never sent.
    let m = read(&bytes_of(&Message::error(
        1,
        ErrorCode::Invalid,
        Some("Not Valid"),
    )))
    .unwrap();
    assert!(matches!(m, Message::Error { name: None, .. }));
}

#[test]
fn ack_round_trip_and_wrong_id() {
    expect_ack(read(&bytes_of(&Message::ack(5))).unwrap(), 5).unwrap();
    assert_eq!(
        expect_ack(read(&bytes_of(&Message::ack(5))).unwrap(), 6).unwrap_err(),
        HandoffError::IdMismatch {
            expected: 6,
            found: 5
        }
    );
    assert_eq!(
        expect_ack(read(&reply_frame(5)).unwrap(), 5).unwrap_err(),
        HandoffError::UnexpectedKind
    );
}

// ---- snapshot ----

fn snapshot_msg(id: u64, entries: &[(&str, Option<&str>)]) -> Message {
    let mut s = Snapshot::new();
    for (name, v) in entries {
        match v {
            Some(v) => s.insert_present(*name, Secret::from_string(*v)).unwrap(),
            None => s.insert_absent(*name).unwrap(),
        }
    }
    Message::reply_snapshot(id, s)
}

#[test]
fn snapshot_states_round_trip() {
    let mut s = Snapshot::new();
    s.insert_present("groq", Secret::from_string("g1")).unwrap();
    s.insert_absent("nvidia").unwrap();
    s.insert_failed("openrouter", ErrorCode::StoreError)
        .unwrap();
    let bytes = bytes_of(&Message::reply_snapshot(4, s));
    let snap = expect_reply(
        read(&bytes).unwrap(),
        4,
        Mode::Snapshot,
        &["groq", "nvidia", "openrouter"],
    )
    .unwrap();
    assert!(matches!(snap.get("groq"), Some(Entry::Present(s)) if s.expose_str().unwrap() == "g1"));
    assert!(matches!(snap.get("nvidia"), Some(Entry::Absent)));
    assert!(matches!(
        snap.get("openrouter"),
        Some(Entry::ReadFailed(ErrorCode::StoreError))
    ));
    // A strict reply may not carry Absent.
    let mut body = vec![0u8, 0, 1, 1, b'a', 1];
    assert!(matches!(
        read(&raw(2, 1, &body)),
        Err(HandoffError::Malformed(_))
    ));
    body[0] = 1;
    assert!(read(&raw(2, 1, &body)).is_ok());
    // A strict view of a snapshot with a non-Present entry is refused.
    let snap = expect_reply(
        read(&bytes).unwrap(),
        4,
        Mode::Snapshot,
        &["groq", "nvidia", "openrouter"],
    )
    .unwrap();
    assert!(snap.into_bundle().is_err());
}

/// HeardRight provider_keys semantics: populate, then omit, then clear (full replacement).
#[test]
fn full_replacement_populates_omits_then_clears() {
    let mut current = SecretBundle::new();
    let apply = |current: &mut SecretBundle, m: Message| {
        let names = ["groq", "nvidia", "openrouter"];
        let snap = expect_reply(m, 1, Mode::Snapshot, &names).unwrap();
        snap.replace_into(current)
    };

    // Populate.
    let m = snapshot_msg(
        1,
        &[
            ("groq", Some("g1")),
            ("nvidia", Some("n1")),
            ("openrouter", Some("o1")),
        ],
    );
    assert!(apply(&mut current, read(&bytes_of(&m)).unwrap()).is_empty());
    assert_eq!(current.len(), 3);
    assert_eq!(current.get("nvidia").unwrap().expose_str().unwrap(), "n1");

    // Omit: nvidia is gone from the owner; the receiver must wipe it, and keep the rest.
    let m = snapshot_msg(
        1,
        &[
            ("groq", Some("g2")),
            ("nvidia", None),
            ("openrouter", Some("o1")),
        ],
    );
    apply(&mut current, read(&bytes_of(&m)).unwrap());
    assert_eq!(
        current.names().collect::<Vec<_>>(),
        vec!["groq", "openrouter"]
    );
    assert_eq!(current.get("groq").unwrap().expose_str().unwrap(), "g2");
    assert!(current.get("nvidia").is_none());

    // A read failure wipes the old value too (fail closed) and is reported.
    let mut s = Snapshot::new();
    s.insert_present("groq", Secret::from_string("g3")).unwrap();
    s.insert_absent("nvidia").unwrap();
    s.insert_failed("openrouter", ErrorCode::StoreError)
        .unwrap();
    let failed = apply(
        &mut current,
        read(&bytes_of(&Message::reply_snapshot(1, s))).unwrap(),
    );
    assert_eq!(
        failed,
        vec![("openrouter".to_string(), ErrorCode::StoreError)]
    );
    assert_eq!(current.names().collect::<Vec<_>>(), vec!["groq"]);

    // Clear.
    let m = snapshot_msg(1, &[("groq", None), ("nvidia", None), ("openrouter", None)]);
    apply(&mut current, read(&bytes_of(&m)).unwrap());
    assert!(current.is_empty());
}

// ---- delivery and acknowledgment ----

#[test]
fn strict_delivery_then_apply_then_matching_ack_is_accepted() {
    // Owner side: a strict reply with a freshly chosen correlation id.
    let id = 0xDEAD_BEEF_u64;
    let wire = bytes_of(&Message::reply_bundle(id, bundle()));
    // Child side: receive keeps the id with the bundle.
    let delivery = receive_delivery(&mut wire.as_slice(), &Limits::default()).unwrap();
    assert_eq!(delivery.id, id);
    // Apply.
    let mut installed = SecretBundle::new();
    for name in ["provider.openai", "provider.deepgram"] {
        installed
            .insert(
                name,
                Secret::from_bytes(delivery.bundle.get(name).unwrap().expose_bytes().to_vec()),
            )
            .unwrap();
    }
    assert_eq!(installed.len(), 2);
    // Ack travels back and the owner accepts the matching id only.
    let ack_wire = bytes_of(&delivery.ack());
    expect_ack(read(&ack_wire).unwrap(), id).unwrap();
    assert_eq!(
        expect_ack(read(&ack_wire).unwrap(), id + 1).unwrap_err(),
        HandoffError::IdMismatch {
            expected: id + 1,
            found: id
        }
    );
}

#[test]
fn strict_receive_refuses_snapshot_and_non_reply_messages() {
    let snap = snapshot_msg(3, &[("groq", Some("g"))]);
    assert_eq!(
        receive_delivery(&mut bytes_of(&snap).as_slice(), &Limits::default()).unwrap_err(),
        HandoffError::ModeMismatch
    );
    let d = receive_snapshot_delivery(&mut bytes_of(&snap).as_slice(), &Limits::default()).unwrap();
    assert_eq!(d.id, 3);
    assert_eq!(d.ack().id(), 3);
    assert_eq!(
        receive_delivery(
            &mut bytes_of(&Message::ack(1)).as_slice(),
            &Limits::default()
        )
        .unwrap_err(),
        HandoffError::UnexpectedKind
    );
}

#[test]
fn snapshot_delivery_apply_then_matching_ack_is_accepted() {
    let id = 0xFEED_u64;
    let wire = bytes_of(&snapshot_msg(id, &[("groq", Some("g1")), ("nvidia", None)]));
    let delivery = receive_snapshot_delivery(&mut wire.as_slice(), &Limits::default()).unwrap();
    // replace_into consumes the snapshot: save the id first, apply, then ack.
    let SnapshotDelivery { id: got, snapshot } = delivery;
    assert_eq!(got, id);
    let mut current = SecretBundle::new();
    assert!(snapshot.replace_into(&mut current).is_empty());
    assert_eq!(current.names().collect::<Vec<_>>(), vec!["groq"]);
    let ack_wire = bytes_of(&Message::ack(got));
    expect_ack(read(&ack_wire).unwrap(), id).unwrap();
}

#[test]
fn policy_accounts_use_the_crate_account_check() {
    for bad in ["", "line\nbreak", "tab\there", "nul\0byte"] {
        assert_eq!(
            HandoffPolicy::new([("ok", bad)]).unwrap_err(),
            HandoffError::BadAccount,
            "{bad:?}"
        );
    }
    assert_eq!(
        HandoffPolicy::new([("ok", "a".repeat(251))]).unwrap_err(),
        HandoffError::BadAccount
    );
    assert!(HandoffPolicy::new([("ok", "a".repeat(250))]).is_ok());
}

// ---- the encoder validates before writing ----

fn encode_err(m: &Message, limits: &Limits) -> HandoffError {
    let mut sink = Vec::new();
    let e = write_message_with_limits(&mut sink, m, limits).unwrap_err();
    assert!(sink.is_empty(), "nothing is written for a refused message");
    e
}

#[test]
fn encoder_refuses_a_256_byte_name() {
    let m = Message::Request {
        id: 1,
        mode: Mode::Strict,
        names: vec!["a".repeat(256)],
    };
    assert_eq!(encode_err(&m, &Limits::default()), HandoffError::BadName);
    let e = Message::Error {
        id: 1,
        code: ErrorCode::Invalid,
        name: Some("a".repeat(256)),
    };
    assert_eq!(encode_err(&e, &Limits::default()), HandoffError::BadName);
}

#[test]
fn encoder_refuses_bad_and_duplicate_request_names() {
    let m = Message::Request {
        id: 1,
        mode: Mode::Strict,
        names: vec!["Upper".into()],
    };
    assert_eq!(encode_err(&m, &Limits::default()), HandoffError::BadName);
    let m = Message::Request {
        id: 1,
        mode: Mode::Strict,
        names: vec!["k".into(), "k".into()],
    };
    assert!(matches!(
        encode_err(&m, &Limits::default()),
        HandoffError::DuplicateName { .. }
    ));
}

#[test]
fn encoder_refuses_too_many_names_including_past_u16() {
    let names: Vec<String> = (0..33).map(|i| format!("n{i}")).collect();
    let m = Message::Request {
        id: 1,
        mode: Mode::Strict,
        names,
    };
    assert!(matches!(
        encode_err(&m, &Limits::default()),
        HandoffError::TooManyNames { limit: 32 }
    ));
    // Limits loose enough that only the u16 wire width stops it.
    let huge = Limits {
        max_frame_bytes: usize::MAX,
        max_names: usize::MAX,
    };
    let names: Vec<String> = (0..=u32::from(u16::MAX)).map(|i| format!("n{i}")).collect();
    let m = Message::Request {
        id: 1,
        mode: Mode::Strict,
        names,
    };
    assert!(matches!(
        encode_err(&m, &huge),
        HandoffError::TooManyNames { .. }
    ));
}

#[test]
fn encoder_refuses_absent_or_failed_entries_in_a_strict_reply() {
    for build in [
        (|s: &mut Snapshot| s.insert_absent("k").unwrap()) as fn(&mut Snapshot),
        |s: &mut Snapshot| s.insert_failed("k", ErrorCode::StoreError).unwrap(),
    ] {
        let mut s = Snapshot::new();
        build(&mut s);
        let m = Message::Reply {
            id: 1,
            mode: Mode::Strict,
            snapshot: s,
        };
        assert!(matches!(
            encode_err(&m, &Limits::default()),
            HandoffError::Malformed(_)
        ));
    }
}

// ---- environment ----

#[test]
fn scrub_inherited_env_removes_ambient_copies() {
    let mut cmd = Command::new("sidecar");
    cmd.env("GROQ_API_KEY", "ambient");
    scrub_inherited_env(&mut cmd, &["GROQ_API_KEY", "OPENAI_API_KEY"]);
    for name in ["GROQ_API_KEY", "OPENAI_API_KEY"] {
        let hit = cmd.get_envs().find(|(k, _)| k.to_str() == Some(name));
        assert!(
            matches!(hit, Some((_, None))),
            "{name} must be explicitly removed"
        );
    }
}

// `MemoryBackend` exists only with debug assertions (see tests/store.rs).
#[cfg(debug_assertions)]
mod policy {
    use std::sync::{Arc, Mutex};

    use rightkit_credentials::handoff::{
        expect_reply, Entry, ErrorCode, HandoffError, HandoffPolicy, Message, Mode,
    };
    use rightkit_credentials::{
        Backend, ConditionalDelete, Config, Interaction, MemoryBackend, Namespace, Result, Secret,
        Store,
    };

    const SERVICE: &str = "dev.rightkit.secrets.test.handoff";

    /// Records the interaction mode of every `get` that reaches the backend.
    struct Recorder {
        inner: MemoryBackend,
        get_modes: Mutex<Vec<Interaction>>,
    }

    impl Backend for Recorder {
        fn name(&self) -> &'static str {
            "recorder"
        }
        fn credential_identity(&self, ns: &Namespace, account: &str) -> Result<String> {
            self.inner.credential_identity(ns, account)
        }
        fn get(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<Option<Secret>> {
            self.get_modes.lock().unwrap().push(mode);
            self.inner.get(ns, account, mode)
        }
        fn exists(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
            self.inner.exists(ns, account, mode)
        }
        fn set(
            &self,
            ns: &Namespace,
            account: &str,
            secret: &Secret,
            mode: Interaction,
        ) -> Result<()> {
            self.inner.set(ns, account, secret, mode)
        }
        fn add_if_absent(
            &self,
            ns: &Namespace,
            account: &str,
            secret: &Secret,
            mode: Interaction,
        ) -> Result<bool> {
            self.inner.add_if_absent(ns, account, secret, mode)
        }
        fn delete(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
            self.inner.delete(ns, account, mode)
        }
        fn delete_if_equals(
            &self,
            ns: &Namespace,
            account: &str,
            expected: &Secret,
            mode: Interaction,
        ) -> Result<ConditionalDelete> {
            self.inner.delete_if_equals(ns, account, expected, mode)
        }
    }

    fn recorder_store() -> (Store, Arc<Recorder>) {
        let rec = Arc::new(Recorder {
            inner: MemoryBackend::new(),
            get_modes: Mutex::new(Vec::new()),
        });
        let store = Store::open(Config::new(Namespace::new(SERVICE).unwrap()).backend(rec.clone()));
        store
            .set("identity-v1", &Secret::from_string("v-identity"))
            .unwrap();
        store
            .set("groq-account", &Secret::from_string("v-groq"))
            .unwrap();
        store
            .set("send.identity", &Secret::from_string("v-wrong-account"))
            .unwrap();
        store
            .set("private-account", &Secret::from_string("v-private"))
            .unwrap();
        (store, rec)
    }

    fn policy() -> HandoffPolicy {
        HandoffPolicy::new([
            ("send.identity", "identity-v1"),
            ("groq", "groq-account"),
            ("nvidia", "nvidia-account"),
        ])
        .unwrap()
    }

    #[test]
    fn reads_the_mapped_account_not_the_logical_name() {
        let (store, _) = recorder_store();
        let got = policy().answer(&store, &["send.identity"]).unwrap();
        assert_eq!(
            got.get("send.identity").unwrap().expose_str().unwrap(),
            "v-identity"
        );
    }

    #[test]
    fn refuses_names_off_the_allowlist_even_when_an_account_exists() {
        let (store, rec) = recorder_store();
        let err = policy()
            .answer(&store, &["send.identity", "private-account"])
            .unwrap_err();
        assert_eq!(
            err,
            HandoffError::NotAllowed {
                name: "private-account".into()
            }
        );
        assert!(
            rec.get_modes.lock().unwrap().is_empty(),
            "nothing is read before validation"
        );
    }

    #[test]
    fn reports_a_missing_name_instead_of_omitting_it() {
        let (store, _) = recorder_store();
        let err = policy().answer(&store, &["groq", "nvidia"]).unwrap_err();
        assert_eq!(
            err,
            HandoffError::NotFound {
                name: "nvidia".into()
            }
        );
    }

    #[test]
    fn refuses_bad_duplicate_and_ill_formed_policy_names() {
        let (store, _) = recorder_store();
        assert_eq!(
            policy().answer(&store, &["Groq"]).unwrap_err(),
            HandoffError::BadName
        );
        assert!(matches!(
            policy().answer(&store, &["groq", "groq"]),
            Err(HandoffError::DuplicateName { .. })
        ));
        assert_eq!(
            HandoffPolicy::new([("Bad Name", "a")]).unwrap_err(),
            HandoffError::BadName
        );
        assert_eq!(
            HandoffPolicy::new([("ok", "")]).unwrap_err(),
            HandoffError::BadAccount
        );
        assert!(matches!(
            HandoffPolicy::new([("ok", "a"), ("ok", "b")]),
            Err(HandoffError::DuplicateName { .. })
        ));
    }

    #[test]
    fn never_prompts_even_when_given_an_interactive_store() {
        let (store, rec) = recorder_store();
        let interactive = store.interactive();
        policy()
            .answer(&interactive, &["send.identity", "groq"])
            .unwrap();
        policy()
            .answer_snapshot(&interactive, &["groq", "nvidia"])
            .unwrap();
        let req = Message::request(1, Mode::Strict, &["groq"]).unwrap();
        let _ = policy().respond(&interactive, &req);
        let modes = rec.get_modes.lock().unwrap();
        assert!(modes.len() >= 5, "reads reached the recording backend");
        assert!(
            modes.iter().all(|m| *m == Interaction::NonInteractive),
            "zero interactive calls"
        );
    }

    #[test]
    fn snapshot_reports_present_absent_and_keeps_optional_names() {
        let (store, _) = recorder_store();
        let snap = policy()
            .answer_snapshot(&store, &["groq", "nvidia"])
            .unwrap();
        assert!(matches!(snap.get("groq"), Some(Entry::Present(_))));
        assert!(matches!(snap.get("nvidia"), Some(Entry::Absent)));
        // Not allowlisted still fails the whole request.
        assert!(matches!(
            policy().answer_snapshot(&store, &["groq", "private-account"]),
            Err(HandoffError::NotAllowed { .. })
        ));
    }

    #[test]
    fn respond_answers_with_the_request_id_or_a_typed_error() {
        let (store, _) = recorder_store();
        let p = policy();
        let ok = p.respond(
            &store,
            &Message::request(5, Mode::Strict, &["groq"]).unwrap(),
        );
        let snap = expect_reply(ok, 5, Mode::Strict, &["groq"]).unwrap();
        assert!(snap.into_bundle().is_ok());

        let missing = p.respond(
            &store,
            &Message::request(6, Mode::Strict, &["nvidia"]).unwrap(),
        );
        assert_eq!(
            expect_reply(missing, 6, Mode::Strict, &["nvidia"]).unwrap_err(),
            HandoffError::Remote {
                code: ErrorCode::NotFound,
                name: Some("nvidia".into())
            }
        );
        let denied = p.respond(
            &store,
            &Message::request(7, Mode::Strict, &["other"]).unwrap(),
        );
        assert_eq!(
            expect_reply(denied, 7, Mode::Strict, &["other"]).unwrap_err(),
            HandoffError::Remote {
                code: ErrorCode::NotAllowed,
                name: Some("other".into())
            }
        );
        let snap = p.respond(
            &store,
            &Message::request(8, Mode::Snapshot, &["groq", "nvidia"]).unwrap(),
        );
        assert!(expect_reply(snap, 8, Mode::Snapshot, &["groq", "nvidia"]).is_ok());
    }
}
