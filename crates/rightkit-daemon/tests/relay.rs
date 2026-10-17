//! `rk-relay/1`: the relay state machine and the device session, wired in process (issue #133).
//! The cipher here is a test double (the real end-to-end suite is an open decision); what is
//! under test is routing, authority, store-and-forward, caps and the blindness of the relay.

use rightkit_daemon::relay::*;
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

const NOW: u64 = 1_800_000_000;

/// XOR with a fixed byte: reversible, and visibly different from the plaintext.
struct Xor(u8);
impl Sealer for Xor {
    fn seal(&self, _to: &MailboxId, plaintext: &[u8]) -> Result<Vec<u8>, SealError> {
        Ok(plaintext.iter().map(|b| b ^ self.0).collect())
    }
    fn open(&self, _from: &MailboxId, sealed: &[u8]) -> Result<Vec<u8>, SealError> {
        Ok(sealed.iter().map(|b| b ^ self.0).collect())
    }
}

fn key() -> Ed25519KeyPair {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap()
}

fn public(pair: &Ed25519KeyPair) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(pair.public_key().as_ref());
    out
}

struct Device {
    session: RelaySession<Xor>,
    conn: ConnId,
}

fn device(owner: &Ed25519KeyPair, conn: ConnId, exp: u64) -> Device {
    let pair = key();
    let credential = issue_ticket(owner, &public(&pair), exp);
    Device {
        session: RelaySession::new(pair, credential, Xor(0x5a), RelayLimits::default()).unwrap(),
        conn,
    }
}

/// Connect and authenticate `d`; returns once the relay said `Authed`.
fn login(core: &mut RelayCore, d: &mut Device) {
    let challenge = core.connect(d.conn, NOW).unwrap();
    let Event::Reply(auth) = d.session.handle(challenge).unwrap() else {
        panic!("expected an auth reply");
    };
    let out = core.handle(d.conn, NOW, auth);
    assert_eq!(out.len(), 1, "{out:?}");
    let Outgoing::Send(_, frame) = &out[0] else {
        panic!("expected a frame");
    };
    assert_eq!(d.session.handle(frame.clone()).unwrap(), Event::Authed);
}

fn frames_for(out: &[Outgoing], conn: ConnId) -> Vec<ServerFrame> {
    out.iter()
        .filter_map(|o| match o {
            Outgoing::Send(c, f) if *c == conn => Some(f.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_connected_recipient_gets_the_message_live_and_ack_clears_the_queue() {
    let owner = key();
    let mut core = RelayCore::new(RelayLimits::default());
    let (mut a, mut b) = (device(&owner, 1, NOW + 3600), device(&owner, 2, NOW + 3600));
    login(&mut core, &mut a);
    login(&mut core, &mut b);

    let (id, put) = a
        .session
        .put(&b.session.mailbox(), Class::Small, b"hello b")
        .unwrap();
    let out = core.handle(a.conn, NOW, put);
    assert!(matches!(&frames_for(&out, 1)[0], ServerFrame::Stored { id: stored, .. } if *stored == id));
    let live = frames_for(&out, 2);
    assert_eq!(live.len(), 1);
    // Blind: the body on the wire is not the plaintext.
    let ServerFrame::Deliver { body, .. } = &live[0] else {
        panic!("expected a delivery");
    };
    assert!(!body.contains("hello"));
    let Event::Message(m) = b.session.handle(live[0].clone()).unwrap() else {
        panic!("expected a message");
    };
    assert_eq!(m.plaintext, b"hello b");
    assert_eq!(m.from, a.session.mailbox());

    let ack = b.session.take_ack().unwrap();
    assert!(core.queued_bytes() > 0);
    let out = core.handle(b.conn, NOW, ack);
    assert_eq!(frames_for(&out, 2), vec![ServerFrame::Acked { count: 1 }]);
    assert_eq!(core.queued_bytes(), 0);
}

#[test]
fn a_sleeping_recipient_collects_stored_messages_later_once() {
    let owner = key();
    let mut core = RelayCore::new(RelayLimits::default());
    let (mut a, mut b) = (device(&owner, 1, NOW + 3600), device(&owner, 2, NOW + 3600));
    login(&mut core, &mut a);
    for text in [&b"one"[..], b"two"] {
        let (_, put) = a.session.put(&b.session.mailbox(), Class::Small, text).unwrap();
        core.handle(a.conn, NOW, put);
    }
    // b was offline; it connects an hour later.
    let later = NOW + 3600 - 1;
    let challenge = core.connect(b.conn, later).unwrap();
    let Event::Reply(auth) = b.session.handle(challenge).unwrap() else {
        panic!()
    };
    core.handle(b.conn, later, auth);
    let out = core.handle(b.conn, later, b.session.fetch(10));
    let got: Vec<Vec<u8>> = frames_for(&out, 2)
        .into_iter()
        .map(|f| match b.session.handle(f).unwrap() {
            Event::Message(m) => m.plaintext,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(got, vec![b"one".to_vec(), b"two".to_vec()]);
    // A second fetch on the same connection returns nothing new.
    assert!(frames_for(&core.handle(b.conn, later, b.session.fetch(10)), 2).is_empty());
    let ack = b.session.take_ack().unwrap();
    core.handle(b.conn, later, ack);
    assert_eq!(core.queued_bytes(), 0);
}

#[test]
fn a_redelivered_message_is_shown_once() {
    let owner = key();
    let mut core = RelayCore::new(RelayLimits::default());
    let (mut a, mut b) = (device(&owner, 1, NOW + 3600), device(&owner, 2, NOW + 3600));
    login(&mut core, &mut a);
    login(&mut core, &mut b);
    let (_, put) = a.session.put(&b.session.mailbox(), Class::Small, b"x").unwrap();
    let out = core.handle(a.conn, NOW, put);
    let deliver = frames_for(&out, 2).remove(0);
    assert!(matches!(b.session.handle(deliver.clone()).unwrap(), Event::Message(_)));
    assert_eq!(b.session.handle(deliver).unwrap(), Event::Duplicate);
}

#[test]
fn a_retried_put_is_stored_once() {
    let owner = key();
    let mut core = RelayCore::new(RelayLimits::default());
    let (mut a, b) = (device(&owner, 1, NOW + 3600), device(&owner, 2, NOW + 3600));
    login(&mut core, &mut a);
    let (_, put) = a.session.put(&b.session.mailbox(), Class::Small, b"once").unwrap();
    let first = core.handle(a.conn, NOW, put.clone());
    let queued = core.queued_bytes();
    let second = core.handle(a.conn, NOW, put);
    assert_eq!(frames_for(&first, 1), frames_for(&second, 1));
    assert_eq!(core.queued_bytes(), queued);
}

#[test]
fn another_circle_cannot_read_or_block_a_mailbox() {
    let (owner, stranger) = (key(), key());
    let mut core = RelayCore::new(RelayLimits::default());
    let (mut a, mut b) = (device(&owner, 1, NOW + 3600), device(&owner, 2, NOW + 3600));
    let mut intruder = device(&stranger, 3, NOW + 3600);
    login(&mut core, &mut a);
    login(&mut core, &mut b);
    login(&mut core, &mut intruder);
    // The intruder deposits to b's mailbox: it goes into the (b, stranger-circle) queue.
    let (_, junk) = intruder
        .session
        .put(&b.session.mailbox(), Class::Small, b"junk")
        .unwrap();
    core.handle(intruder.conn, NOW, junk);
    // b never sees it, and a's own queue to b is unaffected.
    assert!(frames_for(&core.handle(b.conn, NOW, b.session.fetch(10)), 2).is_empty());
    let (_, real) = a.session.put(&b.session.mailbox(), Class::Small, b"real").unwrap();
    let out = core.handle(a.conn, NOW, real);
    assert_eq!(frames_for(&out, 2).len(), 1);
    // The intruder cannot read b's queue either.
    assert!(frames_for(&core.handle(intruder.conn, NOW, intruder.session.fetch(10)), 3).is_empty());
}

#[test]
fn screen_is_direct_only_on_both_sides() {
    let owner = key();
    let mut core = RelayCore::new(RelayLimits::default());
    let (mut a, b) = (device(&owner, 1, NOW + 3600), device(&owner, 2, NOW + 3600));
    login(&mut core, &mut a);
    assert!(matches!(
        a.session.put(&b.session.mailbox(), Class::Screen, b"frame"),
        Err(ClientError::DirectOnly)
    ));
    let forced = ClientFrame::Put {
        id: "00".repeat(16),
        to: b.session.mailbox().to_hex(),
        class: Class::Screen,
        body: "AAAA".into(),
    };
    let out = core.handle(a.conn, NOW, forced);
    assert!(matches!(&frames_for(&out, 1)[0], ServerFrame::Error { code, .. } if code == "direct_only"));
}

#[test]
fn class_caps_and_the_monthly_quota_are_enforced() {
    let owner = key();
    let limits = RelayLimits {
        max_small: 10,
        circle_month_bytes: 25,
        ..RelayLimits::default()
    };
    let mut core = RelayCore::new(limits.clone());
    let pair = key();
    let credential = issue_ticket(&owner, &public(&pair), NOW + 3600);
    let mut a = Device {
        session: RelaySession::new(pair, credential, Xor(1), limits).unwrap(),
        conn: 1,
    };
    let b = device(&owner, 2, NOW + 3600);
    login(&mut core, &mut a);
    // Over the class cap: refused before sealing leaves the device.
    assert!(matches!(
        a.session.put(&b.session.mailbox(), Class::Small, &[0u8; 11]),
        Err(ClientError::TooLarge { .. })
    ));
    for _ in 0..2 {
        let (_, put) = a.session.put(&b.session.mailbox(), Class::Small, &[0u8; 10]).unwrap();
        let out = core.handle(a.conn, NOW, put);
        assert!(matches!(&frames_for(&out, 1)[0], ServerFrame::Stored { .. }));
    }
    let (_, put) = a.session.put(&b.session.mailbox(), Class::Small, &[0u8; 10]).unwrap();
    let out = core.handle(a.conn, NOW, put);
    assert!(matches!(
        &frames_for(&out, 1)[0],
        ServerFrame::Error { code, retry_after: Some(_), .. } if code == "quota_exceeded"
    ));
}

#[test]
fn bad_tickets_and_expired_tickets_are_refused() {
    let (owner, other) = (key(), key());
    let mut core = RelayCore::new(RelayLimits::default());

    // A ticket signed by a key that is not the claimed owner.
    let pair = key();
    let mut credential = issue_ticket(&other, &public(&pair), NOW + 3600);
    credential.owner_pub = public(&owner);
    let mut forged = Device {
        session: RelaySession::new(pair, credential, Xor(1), RelayLimits::default()).unwrap(),
        conn: 1,
    };
    let challenge = core.connect(1, NOW).unwrap();
    let Event::Reply(auth) = forged.session.handle(challenge).unwrap() else {
        panic!()
    };
    let out = core.handle(1, NOW, auth);
    assert!(matches!(&frames_for(&out, 1)[0], ServerFrame::Error { code, .. } if code == "auth_failed"));
    assert!(out.contains(&Outgoing::Close(1)));

    // An expired ticket, and one whose expiry is too far ahead.
    for (conn, exp) in [(2, NOW - 1), (3, NOW + 365 * 24 * 3600)] {
        let mut d = device(&owner, conn, exp);
        let challenge = core.connect(conn, NOW).unwrap();
        let Event::Reply(auth) = d.session.handle(challenge).unwrap() else {
            panic!()
        };
        let out = core.handle(conn, NOW, auth);
        assert!(matches!(&frames_for(&out, conn)[0], ServerFrame::Error { code, .. } if code == "expired"));
    }

    // A frame before authentication.
    core.connect(4, NOW).unwrap();
    let out = core.handle(4, NOW, ClientFrame::Fetch { limit: 1 });
    assert!(matches!(&frames_for(&out, 4)[0], ServerFrame::Error { code, .. } if code == "unauthenticated"));
}

#[test]
fn a_challenge_cannot_be_answered_late_or_replayed_on_another_connection() {
    let owner = key();
    let mut core = RelayCore::new(RelayLimits::default());
    let mut d = device(&owner, 1, NOW + 3600);
    let challenge = core.connect(1, NOW).unwrap();
    let Event::Reply(auth) = d.session.handle(challenge).unwrap() else {
        panic!()
    };
    // Replayed on a second connection (a different nonce): the proof does not verify.
    core.connect(2, NOW).unwrap();
    let out = core.handle(2, NOW, auth.clone());
    assert!(matches!(&frames_for(&out, 2)[0], ServerFrame::Error { code, .. } if code == "auth_failed"));
    // Answered after the challenge window.
    let out = core.handle(1, NOW + 11, auth);
    assert!(matches!(&frames_for(&out, 1)[0], ServerFrame::Error { code, .. } if code == "expired"));
}

#[test]
fn stored_messages_expire() {
    let owner = key();
    let mut core = RelayCore::new(RelayLimits::default());
    let (mut a, b) = (device(&owner, 1, NOW + 3600), device(&owner, 2, NOW + 3600));
    login(&mut core, &mut a);
    let (_, put) = a.session.put(&b.session.mailbox(), Class::Small, b"old").unwrap();
    core.handle(a.conn, NOW, put);
    assert!(core.queued_bytes() > 0);
    core.expire(NOW + 7 * 24 * 3600 + 1);
    assert_eq!(core.queued_bytes(), 0);
}
