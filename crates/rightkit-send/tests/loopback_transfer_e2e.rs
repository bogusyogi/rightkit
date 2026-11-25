//! End to end: two real `Service`s on 127.0.0.1 move a file over real TLS.
//!
//! Each service has its own generated identity, trust file, save folder and listen port (0). The
//! sender learns the receiver through `peer_add_resolved` (no Bonjour, no multicast). The receiver
//! sets the sender to Allow, which skips the owner's card only when the sender's client
//! certificate proves its key during the TLS handshake; a card (`Event::Incoming`) therefore fails
//! the test, and so does a file that lands anywhere but the receiver's own save folder (a sender
//! whose key was not proven is saved under the "unverified" folder).
//!
//! The sender's client identity is process-global (`net::set_client_identity`, set by the last
//! `Service::start`), so the sender is started last.

use rightkit_send::{Config, Event, Identity, Policy, SendItem, Service, TrustState};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(20);

enum Seen {
    Card,
    Finished {
        direction: String,
        state: String,
        saved_to: Option<String>,
        saved_files: Vec<String>,
    },
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Scratch {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rightkit-send-loopback-e2e-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create scratch");
        Scratch(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn config(root: &Path, name: &str) -> Config {
    let base = root.join(name);
    let save_dir = base.join("save");
    std::fs::create_dir_all(&save_dir).expect("create save dir");
    Config {
        product_name: "rightkit-send-e2e".into(),
        alias: name.into(),
        port: 0,
        device_type: "headless".into(),
        device_model: String::new(),
        save_dir,
        trust_path: base.join("trust.json"),
        user_agent: "rightkit-send-e2e".into(),
        quarantine_agent: "rightkit-send-e2e".into(),
        temp_suffix: ".part".into(),
        log_prefix: "e2e:".into(),
        policy: Policy::default(),
    }
}

fn callback(tx: Sender<Seen>) -> Arc<dyn Fn(Event) + Send + Sync> {
    let tx = std::sync::Mutex::new(tx);
    Arc::new(move |event| {
        let seen = match event {
            Event::Incoming(_) => Seen::Card,
            Event::Finished(t) => Seen::Finished {
                direction: t.direction,
                state: t.state,
                saved_to: t.saved_to,
                saved_files: t.saved_files,
            },
            _ => return,
        };
        let _ = tx.lock().expect("tx").send(seen);
    })
}

/// The next `Finished` event, failing on a card or a timeout.
fn next_finished(rx: &Receiver<Seen>, deadline: Instant, who: &str) -> Seen {
    let left = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(left) {
        Ok(Seen::Card) => panic!("{who}: an owner card appeared; the sender's key was not proven"),
        Ok(done) => done,
        Err(_) => panic!("{who}: no finished transfer within {WAIT:?}"),
    }
}

#[test]
fn a_file_crosses_loopback_byte_for_byte_between_two_services() {
    let scratch = Scratch::new();
    let payload: Vec<u8> = (0..70_000u32).map(|i| (i * 31 % 251) as u8).collect();
    let outbox = scratch.0.join("outbox");
    std::fs::create_dir_all(&outbox).expect("create outbox");
    let source = outbox.join("hello.bin");
    std::fs::write(&source, &payload).expect("write source");

    let receiver_id = Identity::generate("receiver").expect("receiver identity");
    let sender_id = Identity::generate("sender").expect("sender identity");
    assert_ne!(receiver_id.fingerprint, sender_id.fingerprint);

    let receiver_config = config(&scratch.0, "receiver");
    let save_dir = receiver_config.save_dir.clone();
    let (receiver_tx, receiver_rx) = mpsc::channel();
    let receiver = Service::start(receiver_config, receiver_id.clone(), callback(receiver_tx))
        .expect("receiver starts");
    // Started last: its identity becomes the process-wide client identity.
    let (sender_tx, sender_rx) = mpsc::channel();
    let sender = Service::start(
        config(&scratch.0, "sender"),
        sender_id.clone(),
        callback(sender_tx),
    )
    .expect("sender starts");
    assert_ne!(receiver.bound_port(), 0);
    assert_ne!(sender.bound_port(), 0);

    receiver
        .set_trust(&sender_id.fingerprint, TrustState::Allow)
        .expect("trust sender");
    sender
        .peer_add_resolved(
            &receiver_id.fingerprint,
            "receiver",
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            receiver.bound_port(),
            &[],
        )
        .expect("add receiver as a peer");

    let transfer = sender
        .send(
            &receiver_id.fingerprint,
            vec![SendItem::Path(source.clone())],
            false,
        )
        .expect("send starts");
    assert!(!transfer.is_empty());

    let deadline = Instant::now() + WAIT;
    let received = next_finished(&receiver_rx, deadline, "receiver");
    let sent = next_finished(&sender_rx, deadline, "sender");

    match sent {
        Seen::Finished {
            direction, state, ..
        } => {
            assert_eq!(direction, "send");
            assert_eq!(state, "done");
        }
        Seen::Card => unreachable!(),
    }
    let (saved_to, saved_files) = match received {
        Seen::Finished {
            direction,
            state,
            saved_to,
            saved_files,
        } => {
            assert_eq!(direction, "receive");
            assert_eq!(state, "done");
            (saved_to.expect("saved_to"), saved_files)
        }
        Seen::Card => unreachable!(),
    };
    assert_eq!(
        std::fs::canonicalize(&saved_to).expect("saved_to exists"),
        std::fs::canonicalize(&save_dir).expect("save dir exists"),
        "the file must land in the receiver's own folder, not the unverified one"
    );
    assert_eq!(saved_files.len(), 1, "exactly one file: {saved_files:?}");
    let landed = Path::new(&saved_files[0]);
    assert_eq!(
        landed.file_name().and_then(|n| n.to_str()),
        Some("hello.bin")
    );
    assert_eq!(
        std::fs::read(landed).expect("read received file"),
        payload,
        "received bytes differ from the sent bytes"
    );
    let partials = std::fs::read_dir(&save_dir)
        .expect("list save dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .count();
    assert_eq!(partials, 0, "temporary files left behind");

    let started = Instant::now();
    sender.stop();
    receiver.stop();
    assert!(started.elapsed() < Duration::from_secs(5), "stop is prompt");
}
