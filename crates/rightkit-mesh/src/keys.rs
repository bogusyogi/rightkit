//! Mailbox key exchange between two paired devices, on ALPN `right/keys/1`.
//!
//! Adrian, 2026-10-17: "per-device keys exchanged at pairing". Pairing needs both devices online,
//! so the first live iroh connection between them runs this exchange: each side sends a fresh
//! random 32-byte key for ITS direction and receives the other's. The connection is authenticated
//! by both device keys and gated by the paired registry, so the keys reach only the paired device.
//! Re-running it with a higher epoch replaces the keys; the previous epoch stays openable so items
//! already in a mailbox still open.
//!
//! Two devices dialling each other at once settle by endpoint id: the lower id's offer wins and the
//! higher id's offer is answered "busy".

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::protocol::{AcceptError, ProtocolHandler};
use serde::{Deserialize, Serialize};

use crate::error::MeshError;
use crate::frame::{read_frame, write_frame};
use crate::identity::PeerTrust;
use crate::seal::{KeyStore, PairKeys};
use crate::util::{random, stream_err, unhex};
use rightkit_fs::digest::hex;

#[derive(Serialize, Deserialize)]
struct Offer {
    epoch: u32,
    send_key: String,
}

#[derive(Serialize, Deserialize)]
struct OfferReply {
    /// `ok`, `stale` (the answerer already holds this epoch or newer) or `busy` (simultaneous dial).
    status: String,
    epoch: u32,
    send_key: String,
}

/// The key exchange state shared by the accepting handler and the dialling side.
#[derive(Clone, Debug)]
pub struct KeyExchange {
    local: [u8; 32],
    keys: Arc<dyn KeyStore>,
    trust: Arc<dyn PeerTrust>,
    inflight: Arc<Mutex<HashSet<[u8; 32]>>>,
}

struct InflightGuard {
    set: Arc<Mutex<HashSet<[u8; 32]>>>,
    peer: [u8; 32],
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.peer);
    }
}

impl KeyExchange {
    pub fn new(local: [u8; 32], keys: Arc<dyn KeyStore>, trust: Arc<dyn PeerTrust>) -> Self {
        Self {
            local,
            keys,
            trust,
            inflight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    fn newest_epoch(&self, peer: &[u8; 32]) -> u32 {
        self.keys.keys(peer).first().map_or(0, |k| k.epoch)
    }

    async fn serve(
        &self,
        peer: [u8; 32],
        send: &mut iroh::endpoint::SendStream,
        recv: &mut iroh::endpoint::RecvStream,
    ) -> Result<(), MeshError> {
        if self.trust.peer(&peer).is_none() {
            return Err(MeshError::NotPaired);
        }
        let (offer, _): (Offer, _) = read_frame(recv, 0).await?;
        let theirs = unhex::<32>(&offer.send_key)?;
        let newest = self.newest_epoch(&peer);
        let we_are_offering = self
            .inflight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&peer);
        let reply = if we_are_offering && self.local < peer {
            OfferReply {
                status: "busy".to_string(),
                epoch: newest,
                send_key: String::new(),
            }
        } else if offer.epoch <= newest {
            OfferReply {
                status: "stale".to_string(),
                epoch: newest,
                send_key: String::new(),
            }
        } else {
            let mine = random::<32>()?;
            self.keys.save(PairKeys {
                peer,
                epoch: offer.epoch,
                send: mine,
                recv: theirs,
            })?;
            OfferReply {
                status: "ok".to_string(),
                epoch: offer.epoch,
                send_key: hex(&mine),
            }
        };
        write_frame(send, &reply, &[]).await
    }

    /// Run the exchange as the dialler over `connection` (ALPN `right/keys/1`) and return the new
    /// keys. If the other side's offer wins a simultaneous dial, wait briefly for it to arrive.
    pub async fn exchange(&self, connection: &Connection) -> Result<PairKeys, MeshError> {
        let peer = *connection.remote_id().as_bytes();
        if self.trust.peer(&peer).is_none() {
            return Err(MeshError::NotPaired);
        }
        let before = self.newest_epoch(&peer);
        self.inflight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(peer);
        let _guard = InflightGuard {
            set: self.inflight.clone(),
            peer,
        };
        let mut epoch = before + 1;
        for _ in 0..2 {
            let mine = random::<32>()?;
            let (mut send, mut recv) = connection.open_bi().await.map_err(stream_err)?;
            let offer = Offer {
                epoch,
                send_key: hex(&mine),
            };
            write_frame(&mut send, &offer, &[]).await?;
            send.finish().map_err(stream_err)?;
            let (reply, _): (OfferReply, _) = read_frame(&mut recv, 0).await?;
            match reply.status.as_str() {
                "ok" => {
                    let keys = PairKeys {
                        peer,
                        epoch,
                        send: mine,
                        recv: unhex::<32>(&reply.send_key)?,
                    };
                    self.keys.save(keys.clone())?;
                    return Ok(keys);
                }
                "stale" => epoch = reply.epoch.max(epoch) + 1,
                _ => break,
            }
        }
        for _ in 0..25 {
            if let Some(newest) = self.keys.keys(&peer).into_iter().next()
                && newest.epoch > before
            {
                return Ok(newest);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(MeshError::NoKeys)
    }
}

impl ProtocolHandler for KeyExchange {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let peer = *connection.remote_id().as_bytes();
        let (mut send, mut recv) = connection.accept_bi().await?;
        self.serve(peer, &mut send, &mut recv)
            .await
            .map_err(AcceptError::from_err)?;
        send.finish()?;
        connection.closed().await;
        Ok(())
    }
}
