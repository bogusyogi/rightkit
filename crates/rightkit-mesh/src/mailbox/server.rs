//! The mailbox as an iroh protocol handler, run by `rightkit-mesh-mailbox` on the Hetzner box.
//!
//! Policy lives in `store.rs`; this file only maps requests to it. The caller's identity is the
//! iroh endpoint id the handshake proved (`Connection::remote_id`): a device can read, ack and
//! configure only the mailbox named by its own id, and can deposit only where the owner allowed it.
//! Events are single JSON lines with counts only (`mbox.deposit`, `mbox.collect`, `mbox.ack`,
//! `mbox.denied`), never ids or sizes per device.

use std::fmt;
use std::sync::Arc;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use rightkit_logs::{EventRecord, Level};
use serde_json::json;

use super::admission::Allowlist;
use super::protocol::{MAX_COLLECT, Reply, Request};
use super::store::Store;
use crate::error::MeshError;
use crate::frame::{read_frame, write_frame};
use crate::util::{now_ms, stream_err};
use rightkit_fs::digest::hex;

/// One structured lifecycle event on stderr (the systemd journal picks it up).
pub fn event(name: &str, count: u64) {
    emit(&EventRecord::new(now_ms() as i64, Level::Info, name).with_field("count", json!(count)));
}

/// Write one lifecycle record as a JSON line on stderr.
pub fn emit(record: &EventRecord) {
    if let Ok(line) = serde_json::to_string(record) {
        eprintln!("{line}");
    }
}

#[derive(Clone)]
pub struct MailboxServer {
    store: Arc<Store>,
    /// When set, only these endpoints may connect at all. Without it any endpoint that reaches the
    /// mailbox may create its own mailbox, which is the only thing keeping the server's storage
    /// bounded then (`Limits::global_bytes`); production runs with the allowlist.
    admission: Option<Arc<Allowlist>>,
}

impl fmt::Debug for MailboxServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MailboxServer")
    }
}

impl MailboxServer {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            admission: None,
        }
    }

    /// Admit only endpoints in `list` (the same file the relay admission service reads).
    pub fn with_admission(mut self, list: Arc<Allowlist>) -> Self {
        self.admission = Some(list);
        self
    }

    async fn serve(
        &self,
        peer: &str,
        send: &mut SendStream,
        recv: &mut RecvStream,
    ) -> Result<(), MeshError> {
        let max = self.store.limits().max_item_bytes;
        let (request, body): (Request, Vec<u8>) = read_frame(recv, max).await?;
        let now = now_ms();
        let store = self.store.clone();
        let owner = peer.to_string();
        match request {
            Request::Deposit { to, record_id } => {
                let size = body.len() as u64;
                let result = tokio::task::spawn_blocking(move || {
                    store.deposit(&owner, &to, &record_id, &body, now)
                })
                .await
                .map_err(stream_err)?;
                let reply = match result {
                    Ok(seq) => {
                        event("mbox.deposit", size);
                        Reply {
                            ok: true,
                            seq: Some(seq),
                            ..Reply::default()
                        }
                    }
                    Err(e) => {
                        event("mbox.denied", 1);
                        Reply::error(e.code, &e.message)
                    }
                };
                write_frame(send, &reply, &[]).await
            }
            Request::Collect { after, limit } => {
                let limit = limit.clamp(1, MAX_COLLECT);
                let result =
                    tokio::task::spawn_blocking(move || store.collect(&owner, after, limit, now))
                        .await
                        .map_err(stream_err)?;
                match result {
                    Ok(items) => {
                        let reply = Reply {
                            ok: true,
                            count: Some(items.len() as u32),
                            ..Reply::default()
                        };
                        write_frame(send, &reply, &[]).await?;
                        event("mbox.collect", items.len() as u64);
                        for (header, sealed) in items {
                            write_frame(send, &header, &sealed).await?;
                        }
                        Ok(())
                    }
                    Err(e) => write_frame(send, &Reply::error(e.code, &e.message), &[]).await,
                }
            }
            Request::Ack { seqs } => {
                let result = tokio::task::spawn_blocking(move || store.ack(&owner, &seqs, now))
                    .await
                    .map_err(stream_err)?;
                let reply = match result {
                    Ok(deleted) => {
                        event("mbox.ack", u64::from(deleted));
                        Reply {
                            ok: true,
                            count: Some(deleted),
                            ..Reply::default()
                        }
                    }
                    Err(e) => Reply::error(e.code, &e.message),
                };
                write_frame(send, &reply, &[]).await
            }
            Request::SetAllow { senders } => {
                let result =
                    tokio::task::spawn_blocking(move || store.set_allow(&owner, &senders, now))
                        .await
                        .map_err(stream_err)?;
                let reply = match result {
                    Ok(()) => Reply::ok(),
                    Err(e) => Reply::error(e.code, &e.message),
                };
                write_frame(send, &reply, &[]).await
            }
        }
    }
}

impl ProtocolHandler for MailboxServer {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let peer = hex(connection.remote_id().as_bytes());
        if self
            .admission
            .as_ref()
            .is_some_and(|list| !list.contains(&peer))
        {
            event("mbox.denied", 1);
            connection.close(1u32.into(), b"not admitted");
            return Ok(());
        }
        while let Ok((mut send, mut recv)) = connection.accept_bi().await {
            if self.serve(&peer, &mut send, &mut recv).await.is_err() {
                break;
            }
            if send.finish().is_err() {
                break;
            }
        }
        Ok(())
    }
}
