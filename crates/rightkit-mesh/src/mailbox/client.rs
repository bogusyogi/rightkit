//! The mailbox client: one cached connection to the mailbox server, one request per stream.

use iroh::endpoint::{Connection, RecvStream};
use iroh::{Endpoint, EndpointAddr};
use tokio::sync::Mutex;

use super::protocol::{ALPN_MAILBOX, DEFAULT_MAX_ITEM_BYTES, ItemHeader, Reply, Request};
use crate::error::MeshError;
use crate::frame::{read_frame, write_frame};
use crate::util::{stream_err, unhex};

/// One item pulled from the mailbox, still sealed.
#[derive(Clone, Debug)]
pub struct MailboxItem {
    /// The server's sequence, for `ack` and for paging.
    pub seq: u64,
    pub from: [u8; 32],
    pub record_id: [u8; 16],
    pub deposited_ms: u64,
    pub sealed: Vec<u8>,
}

pub struct MailboxClient {
    endpoint: Endpoint,
    target: EndpointAddr,
    connection: Mutex<Option<Connection>>,
}

impl MailboxClient {
    pub fn new(endpoint: Endpoint, target: EndpointAddr) -> Self {
        Self {
            endpoint,
            target,
            connection: Mutex::new(None),
        }
    }

    async fn connection(&self, fresh: bool) -> Result<Connection, MeshError> {
        let mut slot = self.connection.lock().await;
        if !fresh
            && let Some(connection) = slot.as_ref()
            && connection.close_reason().is_none()
        {
            return Ok(connection.clone());
        }
        let connection = self
            .endpoint
            .connect(self.target.clone(), ALPN_MAILBOX)
            .await
            .map_err(|e| MeshError::Connect(e.to_string()))?;
        *slot = Some(connection.clone());
        Ok(connection)
    }

    async fn call(&self, request: &Request, body: &[u8]) -> Result<(Reply, RecvStream), MeshError> {
        let mut last = MeshError::NoRelay;
        for attempt in 0..2 {
            let connection = match self.connection(attempt > 0).await {
                Ok(connection) => connection,
                Err(e) => {
                    last = e;
                    continue;
                }
            };
            let outcome: Result<(Reply, RecvStream), MeshError> = async {
                let (mut send, mut recv) = connection.open_bi().await.map_err(stream_err)?;
                write_frame(&mut send, request, body).await?;
                send.finish().map_err(stream_err)?;
                let (reply, _): (Reply, _) = read_frame(&mut recv, 0).await?;
                Ok((reply, recv))
            }
            .await;
            match outcome {
                Ok((reply, recv)) => {
                    if reply.ok {
                        return Ok((reply, recv));
                    }
                    return Err(MeshError::Mailbox {
                        code: reply.code.unwrap_or_default(),
                        message: reply.message.unwrap_or_default(),
                    });
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Deposit one sealed item for `to`. Returns the server's sequence for it.
    pub async fn deposit(
        &self,
        to: &[u8; 32],
        record_id: &[u8; 16],
        sealed: &[u8],
    ) -> Result<u64, MeshError> {
        let request = Request::Deposit {
            to: rightkit_fs::digest::hex(to),
            record_id: rightkit_fs::digest::hex(record_id),
        };
        let (reply, _) = self.call(&request, sealed).await?;
        reply.seq.ok_or(MeshError::Malformed("deposit reply"))
    }

    /// List up to `limit` items after the server sequence `after`.
    pub async fn collect(&self, after: u64, limit: u32) -> Result<Vec<MailboxItem>, MeshError> {
        let (reply, mut recv) = self.call(&Request::Collect { after, limit }, &[]).await?;
        let mut items = Vec::new();
        for _ in 0..reply.count.unwrap_or(0) {
            let (header, sealed): (ItemHeader, _) =
                read_frame(&mut recv, DEFAULT_MAX_ITEM_BYTES).await?;
            items.push(MailboxItem {
                seq: header.seq,
                from: unhex::<32>(&header.from)?,
                record_id: unhex::<16>(&header.record_id)?,
                deposited_ms: header.deposited_ms,
                sealed,
            });
        }
        Ok(items)
    }

    /// Acknowledge items; the server deletes them.
    pub async fn ack(&self, seqs: &[u64]) -> Result<u32, MeshError> {
        let (reply, _) = self
            .call(
                &Request::Ack {
                    seqs: seqs.to_vec(),
                },
                &[],
            )
            .await?;
        Ok(reply.count.unwrap_or(0))
    }

    /// Replace the list of endpoints allowed to deposit into this device's mailbox.
    pub async fn set_allow(&self, senders: &[[u8; 32]]) -> Result<(), MeshError> {
        let senders = senders
            .iter()
            .map(|s| rightkit_fs::digest::hex(s))
            .collect();
        self.call(&Request::SetAllow { senders }, &[]).await?;
        Ok(())
    }
}
