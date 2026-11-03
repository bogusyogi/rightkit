//! The end-to-end encrypted store-and-forward mailbox (Pulse #138, hybrid design).
//!
//! A device that is asleep or off the network cannot be dialled. The sender seals the item to the
//! recipient's pairing key (`seal.rs`) and deposits it at the Orthic mailbox; the recipient dials
//! OUT when it wakes, pulls, opens, handles and acknowledges, and the server deletes on
//! acknowledgement (or after 7 days). The server sees endpoint ids, sizes (padded) and timing,
//! never content. Only small items (64 KB) are stored: images and files go on a live stream
//! (`transfer.rs`), and the relay forwards those without storing them (D7).

pub mod admission;
pub mod client;
pub mod protocol;
pub mod registry;
pub mod server;
pub mod store;

use std::fmt;
use std::sync::Arc;

pub use client::{MailboxClient, MailboxItem};

use crate::caps::check_small;
use crate::error::MeshError;
use crate::identity::{PeerInfo, PeerTrust};
use crate::seal::{KeyStore, open, seal};
use crate::util::random;

/// An item that was opened and is being handed to the application.
#[derive(Clone, Debug)]
pub struct Inbound {
    pub from: PeerInfo,
    pub record_id: [u8; 16],
    /// The sender's per-peer sequence; the application rejects a repeat or a step backwards.
    pub seq: u64,
    pub plaintext: Vec<u8>,
}

/// What one pull did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PullReport {
    /// Opened, handled and acknowledged.
    pub delivered: u32,
    /// From an unknown or revoked sender, or failing authentication; acknowledged to delete them.
    pub dropped: u32,
    /// Left in the mailbox: no key exchanged yet, or the application's handler failed.
    pub held: u32,
}

/// The device side of the mailbox: deposit to a peer, pull from our own.
pub struct Mailbox {
    client: MailboxClient,
    local: [u8; 32],
    trust: Arc<dyn PeerTrust>,
    keys: Arc<dyn KeyStore>,
}

impl fmt::Debug for Mailbox {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Mailbox")
    }
}

impl Mailbox {
    pub fn new(
        client: MailboxClient,
        local: [u8; 32],
        trust: Arc<dyn PeerTrust>,
        keys: Arc<dyn KeyStore>,
    ) -> Self {
        Self {
            client,
            local,
            trust,
            keys,
        }
    }

    /// Tell the server which paired devices may deposit into this device's mailbox. Run after every
    /// pairing, introduction or revocation, and on every start.
    pub async fn sync_allow(&self) -> Result<(), MeshError> {
        let senders: Vec<[u8; 32]> = self
            .trust
            .members()
            .into_iter()
            .map(|m| m.endpoint)
            .collect();
        self.client.set_allow(&senders).await
    }

    /// Seal `plaintext` (at most 64 KB) for the paired device `peer` and deposit it. `seq` is the
    /// caller's persisted per-peer counter. Returns the record id the receipt will name.
    pub async fn deposit(
        &self,
        peer: &[u8; 32],
        seq: u64,
        plaintext: &[u8],
    ) -> Result<[u8; 16], MeshError> {
        check_small(plaintext.len() as u64).map_err(MeshError::Refused)?;
        if self.trust.peer(peer).is_none() {
            return Err(MeshError::NotPaired);
        }
        let keys = self
            .keys
            .keys(peer)
            .into_iter()
            .next()
            .ok_or(MeshError::NoKeys)?;
        let record_id = random::<16>()?;
        let sealed = seal(&keys, &self.local, &record_id, seq, plaintext)?;
        self.client.deposit(peer, &record_id, &sealed).await?;
        Ok(record_id)
    }

    /// Dial out, collect everything waiting, open it and give each item to `handle`. An item is
    /// acknowledged (and so deleted) only after `handle` returned `Ok`.
    pub async fn pull(
        &self,
        mut handle: impl FnMut(Inbound) -> Result<(), MeshError>,
    ) -> Result<PullReport, MeshError> {
        let mut report = PullReport::default();
        let mut after = 0u64;
        loop {
            let page = self.client.collect(after, protocol::MAX_COLLECT).await?;
            let page_len = page.len();
            let mut acks = Vec::new();
            for item in page {
                after = after.max(item.seq);
                let Some(from) = self.trust.peer(&item.from) else {
                    report.dropped += 1;
                    acks.push(item.seq);
                    continue;
                };
                let candidates = self.keys.keys(&item.from);
                match open(
                    &candidates,
                    &item.from,
                    &self.local,
                    &item.record_id,
                    &item.sealed,
                ) {
                    Ok((seq, plaintext)) => {
                        let inbound = Inbound {
                            from,
                            record_id: item.record_id,
                            seq,
                            plaintext,
                        };
                        match handle(inbound) {
                            Ok(()) => {
                                report.delivered += 1;
                                acks.push(item.seq);
                            }
                            Err(_) => report.held += 1,
                        }
                    }
                    Err(MeshError::NoKeys) => report.held += 1,
                    Err(_) => {
                        report.dropped += 1;
                        acks.push(item.seq);
                    }
                }
            }
            if !acks.is_empty() {
                self.client.ack(&acks).await?;
            }
            if page_len < protocol::MAX_COLLECT as usize {
                return Ok(report);
            }
        }
    }
}
