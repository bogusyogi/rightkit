//! `rightkit-mesh-mailbox`: the Orthic end-to-end mailbox server (Pulse #138), run on the Hetzner
//! box beside `iroh-relay` (see `docs/relay-hetzner.md`).
//!
//! It is an iroh endpoint with its own key that connects out to OUR relay and serves ALPN
//! `right/mbox/1`. It stores sealed items it cannot read, for at most seven days, and deletes them
//! when the recipient acknowledges. It holds no device grants and no private keys of any device.
//!
//! Arguments (no defaults for the relay: there is no public relay in this crate):
//!   --data-dir PATH        required; mailboxes live under it
//!   --relay-url URL        required; the relay the mailbox is reached through
//!   --key-file PATH        default DATA_DIR/mailbox.key (32 random bytes, created 0600)
//!   --max-item-kb N        default 128
//!   --ttl-days N           default 7
//!   --queue-mb N           default 50 (per mailbox)
//!   --global-mb N          default 2048 (stored across all mailboxes; a full server refuses deposits)
//!   --allow-file PATH      only endpoints listed in this file (one 64-hex id per line, reloaded on
//!                          change; the file the relay admission service reads) may connect. Strongly
//!                          advised: without it any endpoint that reaches the mailbox may open one.
//!
//! Events are JSON lines on stderr: `mailbox.start`, `mailbox.relay.unreachable`, `mbox.deposit`,
//! `mbox.collect`, `mbox.ack`, `mbox.denied`, `mbox.expire`, `mailbox.stop`. Counts only.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, RelayUrl, SecretKey};
use rightkit_logs::{EventRecord, Level};
use rightkit_mesh::mailbox::admission::Allowlist;
use rightkit_mesh::mailbox::protocol::ALPN_MAILBOX;
use rightkit_mesh::mailbox::server::{MailboxServer, emit, event};
use rightkit_mesh::mailbox::store::{Limits, Store};
use serde_json::json;

struct Args {
    data_dir: PathBuf,
    relay_url: String,
    key_file: PathBuf,
    limits: Limits,
    allow_file: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut data_dir = None;
    let mut relay_url = None;
    let mut key_file = None;
    let mut limits = Limits::default();
    let mut allow_file = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        let number = |text: String| {
            text.parse::<u64>()
                .map_err(|_| format!("{flag}: not a number"))
        };
        match flag.as_str() {
            "--data-dir" => data_dir = Some(PathBuf::from(value()?)),
            "--relay-url" => relay_url = Some(value()?),
            "--key-file" => key_file = Some(PathBuf::from(value()?)),
            "--max-item-kb" => limits.max_item_bytes = number(value()?)? as usize * 1024,
            "--ttl-days" => limits.ttl_ms = number(value()?)? * 24 * 60 * 60 * 1000,
            "--queue-mb" => limits.queue_bytes = number(value()?)? * 1024 * 1024,
            "--global-mb" => limits.global_bytes = number(value()?)? * 1024 * 1024,
            "--allow-file" => allow_file = Some(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let data_dir = data_dir.ok_or("--data-dir is required")?;
    let relay_url = relay_url.ok_or("--relay-url is required")?;
    let key_file = key_file.unwrap_or_else(|| data_dir.join("mailbox.key"));
    Ok(Args {
        data_dir,
        relay_url,
        key_file,
        limits,
        allow_file,
    })
}

/// Read the 32-byte key, or create it owner-only. A short or unreadable file is an error: a new key
/// would silently change the mailbox's identity and strand every device that pinned the old one.
fn load_or_create_key(path: &Path) -> Result<SecretKey, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let seed: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| format!("{}: not a 32-byte key", path.display()))?;
            Ok(SecretKey::from_bytes(&seed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            write_private(path, &key.to_bytes())?;
            Ok(key)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Writes the key owner-only from creation and flushed, and only if no file is there yet: a
/// second writer is refused, never written over.
fn write_private(path: &Path, bytes: &[u8; 32]) -> Result<(), String> {
    use std::io::Write;
    rightkit_fs::atomic_write_with_options(
        path,
        rightkit_fs::AtomicWriteOptions {
            private: true,
            replace: rightkit_fs::ReplaceMode::NoReplace,
            ..Default::default()
        },
        |file| file.write_all(bytes),
    )
    .map_err(|e| e.to_string())
}

async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

async fn run() -> Result<(), String> {
    let args = parse_args()?;
    let relay = RelayUrl::from_str(&args.relay_url).map_err(|e| format!("--relay-url: {e}"))?;
    let secret = load_or_create_key(&args.key_file)?;
    let endpoint_id = rightkit_fs::digest::hex(secret.public().as_bytes());
    let store =
        Arc::new(Store::open(args.data_dir.clone(), args.limits).map_err(|e| e.to_string())?);

    let endpoint = Endpoint::builder(presets::Minimal)
        .secret_key(secret)
        .relay_mode(RelayMode::custom([relay]))
        .bind()
        .await
        .map_err(|e| e.to_string())?;
    let mut server = MailboxServer::new(store.clone());
    if let Some(path) = &args.allow_file {
        server = server.with_admission(Arc::new(Allowlist::open(path)?));
    }
    let router = Router::builder(endpoint)
        .accept(ALPN_MAILBOX, server)
        .spawn();

    if tokio::time::timeout(Duration::from_secs(60), router.endpoint().online())
        .await
        .is_err()
    {
        event("mailbox.relay.unreachable", 1);
    }
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    emit(
        &EventRecord::new(ts_ms, Level::Info, "mailbox.start")
            .with_field("endpoint_id", json!(endpoint_id)),
    );

    let sweeper = {
        let store = store.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(600));
            loop {
                tick.tick().await;
                let store = store.clone();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_millis() as u64);
                if let Ok(expired) = tokio::task::spawn_blocking(move || store.sweep(now)).await {
                    event("mbox.expire", u64::from(expired));
                }
            }
        })
    };

    wait_for_stop().await;
    sweeper.abort();
    let _ = router.shutdown().await;
    event("mailbox.stop", 1);
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rightkit-mesh-mailbox: {message}");
            ExitCode::FAILURE
        }
    }
}
